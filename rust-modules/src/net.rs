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
//! public ABI values (kept here so we do not need the header). TLS peer+host verification is ON
//! (the lab receiver's [`Tls::Pinned`] swaps it for a key pin) with ONE bounded exception on the
//! ordinary request path, [`keypin`] (issue #378): a request whose strict attempt failed with
//! a date verify result (a television with no battery clock) is repeated once, recognising the
//! server by the public key remembered for that exact host and port, with the name check still on.
//! `NOSIGNAL` is set because we call from threads. Response bodies never carry into a log here.
#![allow(non_camel_case_types)]
use std::cell::UnsafeCell;
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

type CURL = c_void;
pub(crate) type curl_slist = c_void;

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
// The four setopt/getinfo wrappers share two C symbols via the macro's `= "name"` override. That
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
    // The four VARIADIC ones, and the `...` marks exactly what `curl.h` marks: the handle and the
    // option id are the only named parameters, and the value arrives through `va_arg`. Spelling
    // that value's type after the ellipsis is what lets one C symbol be bound as more than one
    // wrapper; moving it BEFORE the ellipsis would compile, run on the television, and hand libcurl a
    // garbage pointer on Apple ARM64 (see `dynlib!`'s doc — it is a stack-vs-register convention
    // difference, and it took sign-in down inside `strlen`).
    fn curl_easy_setopt_ptr = "curl_easy_setopt"(handle: *mut CURL, option: c_int, ..., v: *const c_void) -> c_int;
    fn curl_easy_setopt_long = "curl_easy_setopt"(handle: *mut CURL, option: c_int, ..., v: c_long) -> c_int;
    fn curl_easy_getinfo_long = "curl_easy_getinfo"(handle: *mut CURL, info: c_int, ..., out: *mut c_long) -> c_int;
    fn curl_easy_getinfo_ptr = "curl_easy_getinfo"(handle: *mut CURL, info: c_int, ..., out: *mut *const c_void) -> c_int;
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
/// `CURLOPT_FRESH_CONNECT` (LONG + 74) and `CURLOPT_FORBID_REUSE` (LONG + 75): never take a cached
/// connection for this handle, and never leave this handle's connection in the cache. Read off
/// `curl/curl.h`; both have existed since libcurl 7.7. Set only by [`keypin::apply`].
const CURLOPT_FRESH_CONNECT: c_int = 74;
const CURLOPT_FORBID_REUSE: c_int = 75;
const CURLOPT_CONNECTTIMEOUT: c_int = 78;
const CURLOPT_TIMEOUT: c_int = 13;
const CURLOPT_TIMEOUT_MS: c_int = 155;
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
/// one. No private key is in this binary; a pin is a hash of a public key. [`keypin`] uses the
/// same option for the remembered-key fallback (issue #378), on a CA-verified request whose strict
/// attempt failed with a date verify result; it never reuses the lab's `Tls::Pinned`.
const CURLOPT_PINNEDPUBLICKEY: c_int = 10230;

/// `CURLOPT_CAINFO` (OBJECTPOINT + 65) — a path to a PEM bundle to verify the peer against,
/// INSTEAD of whatever trust store this firmware shipped with in 2019.
///
/// **It is not pinning and must not be read as pinning**, which is the confusion
/// [`CURLOPT_PINNEDPUBLICKEY`]'s own doc exists to prevent from the other side: this selects which
/// roots are trusted, and any certificate chaining to one of them still validates. What it buys is
/// independence from a store nobody can update on a television, on a path whose far end is not a
/// Plex service and whose CA may rotate.
pub(crate) const CURLOPT_CAINFO: c_int = 10065;
/// `CURLOPTTYPE_SLISTPOINT + 203`: a `curl_slist` of `host:port:address` entries that pre-populate
/// the DNS cache, so the named host is never resolved. Present since 7.21.3; the entry syntax the
/// television's 7.53.1 parses is documented on [`crate::plex::ResolvePin::entry`]. See [`resolve`].
const CURLOPT_RESOLVE: c_int = 10203;
/// `CURLE_UNKNOWN_OPTION` — what `curl_easy_setopt` answers for an option id this libcurl was
/// built without. The one `setopt` result in this module that is NOT fatal: see [`resolve`].
const CURLE_UNKNOWN_OPTION: c_int = 48;
/// `CURLOPT_CERTINFO` (LONG + 172): ask libcurl to keep the peer's certificate chain, decoded to
/// text, for [`CURLINFO_CERTINFO`] to read after the transfer. libcurl 7.19.1+, the television's
/// is 7.53.1. An option id on the already-bound [`curl_easy_setopt_long`], not a new symbol. It
/// costs libcurl a decode of the WHOLE chain, which is why only a request that wants the peer's
/// key sets it ([`peer_pin_wanted`]).
const CURLOPT_CERTINFO: c_int = 172;
// curl.h info ids (CURLINFO_LONG = 0x200000).
const CURLINFO_RESPONSE_CODE: c_int = 0x20_0002;
/// `CURLINFO_SSL_VERIFYRESULT` (LONG + 13) — the X509 verify result of the peer certificate, as
/// libcurl's TLS backend recorded it: `0` verified, otherwise OpenSSL's `X509_V_ERR_*` number
/// (see [`tls_verify_why`]). An info id on the already-bound [`curl_easy_getinfo_long`], not a
/// new symbol. Backends that never fill it leave `0`, so `0` after a verification failure means
/// "not reported", not "fine".
const CURLINFO_SSL_VERIFYRESULT: c_int = 0x20_000D;
/// `CURLINFO_CERTINFO` (`CURLINFO_SLIST` = 0x400000, + 34) — a `struct curl_certinfo *` for the
/// last connection, read by [`peer_leaf_pin`]. libcurl 7.19.1+. An info id on the already-bound
/// [`curl_easy_getinfo_ptr`], the same C symbol as [`curl_easy_getinfo_long`] spelled with a
/// pointer out-parameter. The list is filled only when [`CURLOPT_CERTINFO`] was set; a backend
/// that does not support it leaves it empty, which reads as "no pin", never as an error. A failed
/// transfer is NOT guaranteed an empty list: libcurl 7.53.1 gathers the chain before it verifies
/// the host name, so a host-mismatch failure leaves it filled. The `rc == 0` guard where
/// [`peer_leaf_pin`] is called is what keeps such a chain from becoming a pin; do not remove it.
const CURLINFO_CERTINFO: c_int = 0x40_0000 + 34;
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
/// `curl_version_info().version_num` (`0xXXYYZZ`), captured once by [`global_init`]; `0` until then
/// or when the struct could not be read. Read by [`resolve`] to pick the entry syntax.
static CURL_VERSION_NUM: AtomicU32 = AtomicU32::new(0);

/// The bound libcurl's numeric version, `0xXXYYZZ`, or `0` before [`global_init`].
pub(crate) fn curl_version_num() -> u32 {
    CURL_VERSION_NUM.load(Ordering::Acquire)
}
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
    let Some(lock) = LEGACY_CRYPTO_LOCKS
        .get()
        .and_then(|locks| locks.get(n as usize))
    else {
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
    version
        .split_ascii_whitespace()
        .any(|part| part.starts_with("OpenSSL/0."))
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
            scope
                .sym("CRYPTO_get_locking_callback")
                .filter(|p| !p.is_null()),
            scope
                .sym("CRYPTO_set_locking_callback")
                .filter(|p| !p.is_null()),
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

pub(crate) fn available() -> bool {
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
                unsafe { std::ffi::CStr::from_ptr(v) }
                    .to_string_lossy()
                    .into_owned()
            };
            let legacy = needs_legacy_crypto_locks(&v);
            let locks = if legacy {
                setup_legacy_crypto_locks(soname)
            } else {
                LegacyCrypto::NotNeeded
            };
            let threaded = threaded_tls_policy(&v, locks);
            CURL_THREADED_TLS_OK.store(threaded, Ordering::Release);
            // `curl_version()`'s prose happened to name c-ares on the development television,
            // but the feature bit is the API. With NOSIGNAL, a synchronous resolver can outlive
            // CONNECTTIMEOUT; log the runtime fact for every firmware instead of promoting one
            // set's string into a fleet-wide guarantee.
            let vi = unsafe { curl_version_info(CURLVERSION_FIRST) };
            if !vi.is_null() {
                CURL_VERSION_NUM.store(unsafe { (*vi).version_num } as u32, Ordering::Release);
            }
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
        BodySink {
            body: Vec::new(),
            max,
            overflowed: false,
        }
    }

    /// Append one curl callback chunk without ever allocating past the caller's ceiling.
    fn push(&mut self, bytes: &[u8]) -> bool {
        if self
            .max
            .is_some_and(|max| bytes.len() > max.saturating_sub(self.body.len()))
        {
            self.overflowed = true;
            return false;
        }
        self.body.extend_from_slice(bytes);
        true
    }
}

/// libcurl `CURLOPT_WRITEFUNCTION`: append received bytes to the caller's bounded sink.
extern "C" fn write_cb(
    ptr: *mut c_char,
    size: usize,
    nmemb: usize,
    userdata: *mut c_void,
) -> usize {
    let n = size.saturating_mul(nmemb);
    if userdata.is_null() || ptr.is_null() {
        return 0;
    }
    let sink = unsafe { &mut *(userdata as *mut BodySink) };
    if sink
        .max
        .is_some_and(|max| n > max.saturating_sub(sink.body.len()))
    {
        sink.overflowed = true;
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, n) };
    if sink.push(slice) {
        n
    } else {
        0
    }
}

struct Easy(*mut CURL);

impl Drop for Easy {
    fn drop(&mut self) {
        unsafe { curl_easy_cleanup(self.0) };
    }
}

/// An owned `curl_slist`, freed on drop. Named for its first job (the request headers) and used
/// for the resolve list too: libcurl does not copy either list, it keeps the pointer for the life
/// of the transfer, so the wrapper must outlive `curl_easy_perform` — which it does by being a
/// local of the function that performs.
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
    /// The peer leaf's `CURLOPT_PINNEDPUBLICKEY` string, present ONLY when the request asked
    /// ([`request_result_evidence`]'s `learn_pin`) and [`peer_pin_wanted`] allowed it: an https
    /// URL, strict verification, no redirects. A transfer that failed has no `Resp`, so this is
    /// never a pin from a connection that did not complete.
    pub peer_pin: Option<String>,
}
impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The failure fact libcurl exposes at the request boundary. `TimedOut` is intentionally not yet
/// called a caller deadline: curl uses code 28 for both its connect ceiling and its whole-request
/// ceiling, so the layer which selected those clocks decides whether that timer was the caller's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    TimedOut,
    Transport,
}

/// Safe response evidence without a partial body, URL, headers or arbitrary error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RequestFailure {
    pub cause: RequestError,
    pub status: Option<u16>,
    pub body_limit: Option<usize>,
    /// The non-zero `CURLcode` `curl_easy_perform` returned, when the failure came from a transfer
    /// at all. `None` for a request refused before libcurl ran (a URL with a NUL, an unloadable
    /// libcurl, a failed handle setup). A bare number with no identity, which is why it may leave
    /// the device in `telemetry::incident`'s link class; a DNS failure and a TLS refusal are the
    /// two answers a failed sign-in most needs told apart.
    pub curl_rc: Option<i32>,
}

impl From<RequestError> for RequestFailure {
    fn from(cause: RequestError) -> Self { Self { cause, status: None, body_limit: None, curl_rc: None } }
}

/// CURLINFO_RESPONSE_CODE is the last response, not the CONNECT proxy response:
/// https://curl.se/libcurl/c/CURLINFO_RESPONSE_CODE.html . On errors following redirects we
/// cannot prove it belongs to the final origin, so withhold it. Only documented body-transfer
/// failures retain final HTTP evidence; TLS, setup and unrecognized failures cannot earn it.
/// Codes: https://curl.se/libcurl/c/libcurl-errors.html (partial file, write callback, timeout,
/// receive failure, HTTP/2 connection/stream errors). In curl-8_7_1/lib/http2.c,
/// http2_handle_stream_close returns 92 on an error reset; cf_h2_recv returns 16 on a closed
/// connection before body bytes, even after headers published status in lib/http.c.
/// Its receive loop also flushes H2 control frames through h2_progress_egress, which can return
/// CURLE_SEND_ERROR (55); a send error alone does not imply that no response was received.
/// Setup can also return 16, so valid final status remains mandatory; no code alone is evidence.
/// No new curl option/info constant or binding is required.
fn response_status(rc: c_int, info_rc: c_int, code: c_long, follow_redirects: bool) -> Option<u16> {
    if info_rc != 0 || !(100..=599).contains(&code) { return None; }
    if rc == 0 { return u16::try_from(code).ok(); }
    if !follow_redirects && code >= 200 && matches!(rc, 16 | 18 | 23 | 28 | 55 | 56 | 92) {
        return u16::try_from(code).ok();
    }
    None
}

fn finish_response(
    rc: c_int, info_rc: c_int, code: c_long, follow_redirects: bool,
    max_body: Option<usize>, sink: BodySink,
) -> Result<Resp, RequestFailure> {
    let status = response_status(rc, info_rc, code, follow_redirects);
    if sink.overflowed || rc != 0 || status.is_none() {
        return Err(RequestFailure {
            cause: if rc == 28 && !sink.overflowed { RequestError::TimedOut } else { RequestError::Transport },
            status,
            body_limit: if sink.overflowed { max_body } else { None },
            curl_rc: (rc != 0).then_some(rc as i32),
        });
    }
    Ok(Resp { status: status.unwrap(), body: sink.body, peer_pin: None })
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
    /// Millisecond whole-request deadline when non-zero. It takes precedence over `total_s` and
    /// lets an ABR transaction spend its exact remaining reserve rather than rounding it up to a
    /// whole generic API second.
    pub total_ms: c_long,
    pub low_speed_bps: c_long,
    pub low_speed_s: c_long,
}

/// The deadlines for an **API call** — a request whose answer is small and whose caller is a user
/// waiting behind a spinner. The values every call in this app used before they were a parameter,
/// unchanged, so nothing about plex.tv sign-in moves.
pub const API: Timeouts = Timeouts {
    connect_s: 8,
    total_s: 25,
    total_ms: 0,
    low_speed_bps: 0,
    low_speed_s: 0,
};

/// The deadlines for a PMS body whose size is content-dependent (library JSON, artwork, sidecar
/// subtitles). A connect normally costs at most 8 s and fewer than one byte per second for 30 s
/// ends a stalled transfer, but a healthy transfer has no wall-clock guillotine:
/// `CURLOPT_TIMEOUT=0` is libcurl's documented disabled value.
pub const BULK: Timeouts = Timeouts {
    connect_s: 8,
    total_s: 0,
    total_ms: 0,
    low_speed_bps: 1,
    low_speed_s: 30,
};

/// Protocol floor for a public redirect. A TLS request may remain TLS only; a plaintext request
/// may stay plaintext or upgrade. Pure so the no-downgrade rule is host-testable.
fn allowed_redirect_protocols(url: &[u8]) -> c_long {
    if url
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"https://"))
    {
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn request(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
    resolve: Option<&str>,
) -> Option<Resp> {
    request_result(url, headers, verb, body, t, follow_redirects, max_body, resolve).ok()
}

/// Typed twin used by a caller which must keep curl's timeout distinct from every other transport
/// failure. Ordinary clients retain [`request`]'s compatibility `Option`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_result(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
    resolve: Option<&str>,
) -> Result<Resp, RequestError> {
    request_result_evidence(url, headers, verb, body, t, follow_redirects, max_body, resolve, false)
        .map_err(|failure| failure.cause)
}

/// [`request_result`] keeping the whole [`RequestFailure`] — the `CURLcode` a failed request's
/// evidence names. The same one entry every such request passes.
///
/// `learn_pin` asks for [`Resp::peer_pin`]: the identity probe sets it, so a verified connection
/// teaches the app the server's public key, and nothing else does (it makes libcurl decode the
/// whole chain).
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_result_evidence(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
    resolve: Option<&str>,
    learn_pin: bool,
) -> Result<Resp, RequestFailure> {
    // A host test that needs to drive the REAL discovery-probe path (`auth::get_identity`, and
    // through it `http::request_probe` or `http::request_probe_learning_key`) against a loopback
    // HTTPS server cannot make libcurl trust that server's self-signed certificate any other way: this function is the one place every
    // such request enters (see `request_tls_evidence`'s `nowan` comment for the same observation
    // about the offline gate). `test_ca_bundle::get()` is compiled out entirely in a non-test
    // build — there is no bundle to read and no branch that reads one — so this is not a
    // production bypass, only a second `cfg(test)` caller of the `Tls::CaBundle` mode that already
    // exists for the lab receiver.
    #[cfg(test)]
    if let Some(bundle) = test_ca_bundle::get() {
        return request_tls_evidence(
            url,
            headers,
            verb,
            body,
            t,
            follow_redirects,
            max_body,
            Tls::CaBundle(&bundle),
            resolve,
            learn_pin,
        );
    }
    request_tls_evidence(
        url,
        headers,
        verb,
        body,
        t,
        follow_redirects,
        max_body,
        Tls::Ca,
        resolve,
        learn_pin,
    )
}

/// Test-only override of the CA trust root `request_result` verifies against — see that
/// function's doc. Process-global rather than thread-local: `auth::race_batch` dials each
/// candidate on a real worker thread (`task::spawn_small`), which a thread-local would never see.
/// Guard every read/write with `crate::testlock::serial()`, matching this crate's existing
/// convention for shared test-global state (see `lib.rs`'s `testlock` module doc).
#[cfg(test)]
pub(crate) mod test_ca_bundle {
    use std::sync::Mutex;

    static BUNDLE: Mutex<Option<String>> = Mutex::new(None);

    /// Point every `request_result` call at `path` (a PEM CA bundle) until cleared. Caller must
    /// hold `crate::testlock::serial()` for the duration any dial using it can run.
    pub(crate) fn set(path: Option<&str>) {
        *BUNDLE.lock().unwrap_or_else(|e| e.into_inner()) = path.map(str::to_owned);
    }

    pub(crate) fn get() -> Option<String> {
        BUNDLE.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Real loopback PMS doubles for driving the discovery-probe race through the REAL curl/TLS
/// stack (`auth::get_identity` → `http::request_probe` or `request_probe_learning_key` →
/// `net::request_result_evidence`, unmodified) rather
/// than a fake [`auth::ProbeDial`] closure. `test_ca_bundle` above is the other half: it is how
/// curl is told to trust the certificate [`mint_cert`] mints here — the same PEM, so a real TLS
/// handshake against [`spawn_dual_protocol`] genuinely verifies.
// Not `pub(crate) mod` directly: `ci/check-deps.sh`'s `threads` gate only recognises a bare
// `#[cfg(test)]` + `mod ` pair when deciding a block is test-only and skipping the real
// `std::thread::spawn` calls inside it (`spawn_dual_protocol`/`spawn_plain_only`, standing in for
// a loopback PMS peer) — the same convention the `mutators` gate above already relies on. A `pub`
// or `pub(crate)` qualifier on the `mod` line does not match that pattern, so the module stays
// private and every item the rest of the crate needs is re-exported below instead.
#[cfg(test)]
mod loopback_pms {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Once};

    /// A minted self-signed certificate, the key rustls needs to terminate TLS with it, and its
    /// PEM form for `test_ca_bundle::set`.
    pub(crate) struct TestCert {
        cert_der: rustls::pki_types::CertificateDer<'static>,
        key_der: rustls::pki_types::PrivateKeyDer<'static>,
        pub(crate) pem: String,
        /// The DER `SubjectPublicKeyInfo` of the SERVED leaf's key pair, taken from the key pair
        /// itself rather than parsed back out of the certificate — the independent half of a
        /// pin check (`spki::pin_from_spki_der`).
        pub(crate) spki_der: Vec<u8>,
        /// Certificates the server sends AFTER the leaf (the issuing CA, for
        /// [`TestCert::serving_chain`]); empty unless asked, which is the shape a minted self-signed
        /// certificate has.
        chain_tail: Vec<rustls::pki_types::CertificateDer<'static>>,
        /// The issuing CA of a [`mint_ca_issued_cert`] leaf, kept so the chain can be served.
        issuer_der: Option<rustls::pki_types::CertificateDer<'static>>,
    }

    impl TestCert {
        /// This certificate served as the chain `[leaf, issuer]`, the way a real server sends its
        /// leaf with the CA that signed it. Only a [`mint_ca_issued_cert`] has an issuer.
        pub(crate) fn serving_chain(mut self) -> TestCert {
            let issuer = self.issuer_der.clone().expect("only a CA-issued leaf has an issuer to serve");
            self.chain_tail = vec![issuer];
            self
        }
    }

    /// Mint a self-signed cert whose SAN list is exactly `names`. rcgen tells a dotted IPv4
    /// literal apart from a DNS label itself, so a caller passing `["127.0.0.1"]` gets an IP SAN
    /// and one passing a `plex.direct`-shaped dashed label gets a DNS SAN — the E2E tests need
    /// both, one per candidate that dials this loopback double a different way.
    pub(crate) fn mint_cert(names: &[&str]) -> TestCert {
        let subject_alt_names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(subject_alt_names).expect("test cert generation");
        let pem = cert.pem();
        let cert_der = cert.der().clone();
        let spki_der = rcgen::PublicKeyData::subject_public_key_info(&signing_key);
        let key_der = rustls::pki_types::PrivateKeyDer::from(signing_key);
        TestCert {
            cert_der,
            key_der,
            pem,
            spki_der,
            chain_tail: Vec::new(),
            issuer_der: None,
        }
    }

    /// Mint a CA and a leaf it issues, the leaf valid exactly over `[not_before, not_after]`
    /// (`(year, month, day)`), the SAN list exactly `names`. `pem` is the **CA**: that is what a
    /// client is told to trust, and the server presents only the leaf (the shape of a real
    /// `*.plex.direct` certificate) unless [`TestCert::serving_chain`] is asked for — and the
    /// only way to give a test a leaf whose validity window excludes "now" (a wrong television clock) while the trust anchor stays valid.
    pub(crate) fn mint_ca_issued_cert(
        names: &[&str],
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
    ) -> TestCert {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        // Distinct subjects: rcgen defaults both to the same CN, which makes OpenSSL read the leaf
        // as self-signed ("error 18") and refuse to chain it to the CA.
        ca_params.distinguished_name.push(rcgen::DnType::CommonName, "PlxNative test CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params.self_signed(&ca_key).expect("ca cert");

        let leaf_key = KeyPair::generate().expect("leaf key");
        let mut leaf_params =
            CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .expect("leaf params");
        leaf_params.distinguished_name.push(rcgen::DnType::CommonName, "plex.direct test leaf");
        leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        leaf_params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
        leaf_params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).expect("leaf cert");
        TestCert {
            cert_der: leaf.der().clone(),
            spki_der: rcgen::PublicKeyData::subject_public_key_info(&leaf_key),
            key_der: rustls::pki_types::PrivateKeyDer::from(leaf_key),
            pem: ca.pem(),
            chain_tail: Vec::new(),
            issuer_der: Some(ca.der().clone()),
        }
    }

    /// `(year, month, day)` of `days` from now (negative: the past), in the shape
    /// [`mint_ca_issued_cert`] takes — dates relative to now, so a test does not rot.
    pub(crate) fn ymd_from_now(days: i64) -> (i32, u8, u8) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after 1970")
            .as_secs() as i64;
        let (y, m, d) = super::civil_date(now + days * 86_400);
        (y as i32, m as u8, d as u8)
    }

    /// RAII guard for the process-global CA override (`test_ca_bundle`): writes `pem` to a scratch
    /// file, installs it as curl's trusted CAINFO for the duration, and always clears the override
    /// (and deletes the file) on drop — including on panic/unwind — so a failing assertion in one
    /// test can never leak a trusted CA into another running after it under the same
    /// `testlock::serial()` guard.
    pub(crate) struct TestCaGuard(std::path::PathBuf);

    impl TestCaGuard {
        pub(crate) fn install(pem: &str, tag: &str) -> TestCaGuard {
            let path = std::env::temp_dir().join(format!(
                "plxnative-test-ca-{tag}-{}-{:?}.pem",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, pem).expect("write scratch CA bundle");
            let path_str = path.to_string_lossy().into_owned();
            super::test_ca_bundle::set(Some(&path_str));
            TestCaGuard(path)
        }
    }

    impl Drop for TestCaGuard {
        fn drop(&mut self) {
            super::test_ca_bundle::set(None);
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn ring_provider_once() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn tls_config(cert: &TestCert) -> Arc<rustls::ServerConfig> {
        ring_provider_once();
        let mut certs = vec![cert.cert_der.clone()];
        certs.extend(cert.chain_tail.iter().cloned());
        let key = cert.key_der.clone_key();
        let cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("test tls server config");
        Arc::new(cfg)
    }

    fn http_ok(body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// Drain one HTTP/1.1 request off `r` before answering — the probe's `/identity` GET has no
    /// body, so this only needs to find the header terminator, not parse anything. Bounded and
    /// best-effort: nothing here sends a request built to defeat it.
    fn drain_request(r: &mut impl Read) {
        let mut buf = [0u8; 4096];
        let mut seen = Vec::new();
        loop {
            let Ok(n) = r.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            seen.extend_from_slice(&buf[..n]);
            if seen.windows(4).any(|w| w == b"\r\n\r\n") || seen.len() > 64 * 1024 {
                return;
            }
        }
    }

    /// A loopback PMS double that answers the SAME body over either transport on ONE port —
    /// exactly what `plex::probe::candidates` assumes when it synthesizes a plaintext twin at the
    /// advertised connection's own address and port. Peeks the first byte: `0x16` is a TLS
    /// handshake record, anything else is treated as plaintext HTTP.
    pub(crate) fn spawn_dual_protocol(cert: Arc<TestCert>, body: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind dual-protocol listener");
        let port = listener.local_addr().unwrap().port();
        let tls_cfg = tls_config(&cert);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { continue };
                let tls_cfg = Arc::clone(&tls_cfg);
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut peek = [0u8; 1];
                    let is_tls = matches!(sock.peek(&mut peek), Ok(1) if peek[0] == 0x16);
                    if is_tls {
                        let Ok(mut conn) = rustls::ServerConnection::new(tls_cfg) else {
                            return;
                        };
                        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                        drain_request(&mut tls);
                        let _ = tls.write_all(&http_ok(&body));
                        let _ = tls.flush();
                    } else {
                        drain_request(&mut sock);
                        let _ = sock.write_all(&http_ok(&body));
                    }
                });
            }
        });
        port
    }

    /// What [`spawn_observed`] saw: the connections it accepted (every TCP accept, including one
    /// whose TLS handshake the client then abandoned) and every complete request it read, headers
    /// and body, verbatim.
    pub(crate) struct Observed {
        pub(crate) port: u16,
        pub(crate) accepted: Arc<std::sync::atomic::AtomicUsize>,
        pub(crate) requests: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl Observed {
        /// TCP connections accepted so far.
        pub(crate) fn accepted(&self) -> usize {
            self.accepted.load(std::sync::atomic::Ordering::Acquire)
        }
    }

    /// One whole HTTP/1.1 request off `r`: the head, then `Content-Length` bytes of body.
    fn read_whole_request(r: &mut impl Read) -> Vec<u8> {
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match r.read(&mut buf) {
                Ok(0) | Err(_) => return seen,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
            if seen.len() > 64 * 1024 {
                return seen;
            }
        };
        let head = String::from_utf8_lossy(&seen[..head_end]).to_ascii_lowercase();
        let want = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        while seen.len() < head_end + want {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        }
        seen
    }

    /// A TLS-only loopback double that COUNTS and RECORDS: the latch tests count the handshakes a
    /// request cost, the body test reads what arrived. Answers `body` with `200`, or the tail of it
    /// with `206` and a `Content-Range` when the request carries `Range: bytes=N-`, so a
    /// [`crate::curlio::CurlSource`] can open, seek and reopen against it. One request per
    /// connection (`Connection: close`); [`spawn_observed_keepalive`] is the persistent twin.
    pub(crate) fn spawn_observed(cert: Arc<TestCert>, body: Vec<u8>) -> Observed {
        spawn_observed_conn(cert, body, false)
    }

    /// [`spawn_observed`] speaking persistent HTTP/1.1: every reply carries a `Content-Length` and
    /// no `Connection: close`, and a connection serves requests until the client closes it. What a
    /// real media server does, and the only double in which a libcurl connection cache can be seen
    /// handing one connection to a second transfer.
    pub(crate) fn spawn_observed_keepalive(cert: Arc<TestCert>, body: Vec<u8>) -> Observed {
        spawn_observed_conn(cert, body, true)
    }

    fn spawn_observed_conn(cert: Arc<TestCert>, body: Vec<u8>, keep_alive: bool) -> Observed {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind observed listener");
        let port = listener.local_addr().unwrap().port();
        let tls_cfg = tls_config(&cert);
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (count, log) = (Arc::clone(&accepted), Arc::clone(&requests));
        let connection = if keep_alive { "" } else { "Connection: close\r\n" };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { continue };
                count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let (tls_cfg, body, log) = (Arc::clone(&tls_cfg), body.clone(), Arc::clone(&log));
                std::thread::spawn(move || {
                    let Ok(mut conn) = rustls::ServerConnection::new(tls_cfg) else { return };
                    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
                    loop {
                        let request = read_whole_request(&mut tls);
                        if request.is_empty() {
                            return;
                        }
                        let head = String::from_utf8_lossy(&request).to_ascii_lowercase();
                        let start = head
                            .lines()
                            .find_map(|l| l.strip_prefix("range: bytes="))
                            .and_then(|v| v.split('-').next())
                            .and_then(|v| v.trim().parse::<usize>().ok());
                        log.lock().unwrap_or_else(|e| e.into_inner()).push(request);
                        let reply = match start {
                            Some(at) if at < body.len() => {
                                let tail = &body[at..];
                                let mut out = format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {at}-{}/{}\r\n{connection}\r\n",
                                    tail.len(),
                                    body.len() - 1,
                                    body.len()
                                )
                                .into_bytes();
                                out.extend_from_slice(tail);
                                out
                            }
                            _ => {
                                let mut out = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{connection}\r\n",
                                    body.len()
                                )
                                .into_bytes();
                                out.extend_from_slice(&body);
                                out
                            }
                        };
                        if tls.write_all(&reply).is_err() || tls.flush().is_err() || !keep_alive {
                            return;
                        }
                    }
                });
            }
        });
        Observed { port, accepted, requests }
    }

    /// A loopback double that only ever speaks plaintext HTTP — for the "HTTPS fails" E2E
    /// scenario, where a TLS ClientHello against this listener must fail the handshake (there is
    /// no `rustls::ServerConnection` here to answer it) while a plain request still succeeds.
    pub(crate) fn spawn_plain_only(body: Vec<u8>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind plaintext listener");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { continue };
                let body = body.clone();
                std::thread::spawn(move || {
                    drain_request(&mut sock);
                    let _ = sock.write_all(&http_ok(&body));
                });
            }
        });
        port
    }

    /// A loopback port nothing listens on: bind, read back the ephemeral port, then drop the
    /// listener — so a candidate dialled here gets a deterministic refused connection rather than
    /// a merely-unassigned one.
    pub(crate) fn dead_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind dead-port probe");
        listener.local_addr().unwrap().port()
    }
}
#[cfg(test)]
pub(crate) use loopback_pms::{dead_port, mint_ca_issued_cert, mint_cert, spawn_dual_protocol, spawn_observed, spawn_observed_keepalive, spawn_plain_only, ymd_from_now, TestCaGuard, TestCert};

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
    request_tls_result(url, headers, verb, body, t, follow_redirects, max_body, tls, None).ok()
}

/// `resolve` is a ready-made `CURLOPT_RESOLVE` entry (`host:port:address`, see
/// [`crate::plex::ResolvePin::entry`]) for the URL's own host, or `None` to let the resolver
/// answer. Every request whose origin carries a [`crate::plex::ResolvePin`] passes one; plex.tv
/// calls pass `None`, and that is the difference the `nowan` trigger grades (see [`refuse_name`]).
#[allow(clippy::too_many_arguments)]
fn request_tls_result(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
    tls: Tls<'_>,
    resolve: Option<&str>,
) -> Result<Resp, RequestError> {
    request_tls_evidence(url, headers, verb, body, t, follow_redirects, max_body, tls, resolve, false)
        .map_err(|failure| failure.cause)
}

/// Opt-in detailed twin; compatibility callers project only the original cause above.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_evidence(
    url: &str, headers: &[String], verb: &str, body: Option<&[u8]>, t: Timeouts,
    follow_redirects: bool, max_body: Option<usize>, resolve: Option<&str>,
) -> Result<Resp, RequestFailure> {
    request_tls_evidence(url, headers, verb, body, t, follow_redirects, max_body, Tls::Ca, resolve, false)
}

#[allow(clippy::too_many_arguments)]
fn request_tls_evidence(
    url: &str, headers: &[String], verb: &str, body: Option<&[u8]>, t: Timeouts,
    follow_redirects: bool, max_body: Option<usize>, tls: Tls<'_>, resolve: Option<&str>,
    learn_pin: bool,
) -> Result<Resp, RequestFailure> {
    // Every fallible CString is built BEFORE the easy handle exists. The RAII guards below still
    // make later early returns safe, but this ordering also means malformed caller input never
    // enters curl with a half-configured request.
    let verb_c = CString::new(verb).map_err(|_| RequestError::Transport)?;
    let url_c = CString::new(url).map_err(|_| RequestError::Transport)?;
    let resolve_c = resolve
        .map(CString::new)
        .transpose()
        .map_err(|_| RequestError::Transport)?;
    // The offline reproduction: with `/tmp/plxnative-nowan` armed, a name reaches the wire only
    // with a pin. This is the ONE place every easy request passes (`request_result` enters here
    // directly), which is why the gate is here and not on `request_tls`.
    if resolve.is_none() && refuse_name(crate::plex::url_host(url), t.connect_s) {
        return Err(RequestError::Transport.into());
    }
    let ua =
        CString::new(crate::plex::identity::user_agent()).map_err(|_| RequestError::Transport)?;
    let verified_https = peer_pin_wanted(url, &tls, follow_redirects);
    let read_peer_pin = learn_pin && verified_https;
    let tls_c = match tls {
        Tls::Ca => TlsCfg::Ca,
        Tls::CaBundle(p) => TlsCfg::CaBundle(CString::new(p).map_err(|_| RequestError::Transport)?),
        Tls::Pinned(p) => TlsCfg::Pinned(CString::new(p).map_err(|_| RequestError::Transport)?),
    };
    let hdr_owned: Vec<CString> = headers
        .iter()
        .map(|line| CString::new(line.as_str()))
        .collect::<Result<_, _>>()
        .map_err(|_| RequestError::Transport)?;
    // The guard `CURL_OK` exists for. Without it, a device with no libcurl this app can bind
    // reaches `curl_easy_init`'s wrapper and takes `dynlib::missing_symbol`, which panics — an
    // account lookup failing should return None and let the caller fall back, not kill a thread.
    if !available() {
        return Err(RequestError::Transport.into());
    }
    // A legacy OpenSSL whose callback API is unexpectedly hidden can still support HTTPS control,
    // but only one easy request at a time. The normal installed/existing-callback path never takes
    // this mutex, and curlio remains disabled in the degraded state.
    let _fallback_serial = if threaded_tls_ready() {
        None
    } else {
        Some(
            CURL_FALLBACK_SERIAL
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    };
    // **Key mode (issue #378) is only ever considered for a strictly verified, redirect-free https
    // request** — never [`Tls::Pinned`] (which has already turned verification off for a reason of
    // its own), never plaintext. `key` is that request's `host:port` in [`keypin`]'s tables, or
    // `None` when the request can never use them.
    let key = verified_https.then(|| keypin::key_of_url(url)).flatten();
    let mut mode = key.as_deref().map_or(keypin::Mode::Strict, keypin::begin);
    // A strict attempt that failed with a date verify result, kept while key mode is tried: if libcurl
    // cannot even be put in key mode, the request fails as it would have.
    let mut held: Option<Attempt> = None;
    // **One attempt = one fresh easy handle, one fresh response sink, the same inputs.** A retry
    // therefore cannot inherit per-attempt state (a partly filled body buffer, a read offset, a
    // header list the first handle owned): a failed TLS handshake sent nothing, and the second
    // attempt starts from nothing too. `body` is referenced, never consumed.
    // `Ok(None)` is a libcurl that would not be put in key mode: nothing was sent.
    let attempt = |mode: &keypin::Mode, t: Timeouts| -> Result<Option<Attempt>, RequestFailure> {
        let read_peer_pin = read_peer_pin && matches!(mode, keypin::Mode::Strict);
        let pin_c = match mode {
            keypin::Mode::Key { pin, .. } => {
                Some(CString::new(pin.as_str()).map_err(|_| RequestError::Transport)?)
            }
            keypin::Mode::Strict => None,
        };
        unsafe {
            macro_rules! require_setopt {
                ($call:expr, $name:literal) => {{
                    let rc = $call;
                    if rc != 0 {
                        crate::log(&format!(
                            "net: libcurl refused security option {} (rc={rc}); request cancelled",
                            $name
                        ));
                        return Err(RequestError::Transport.into());
                    }
                }};
            }
            let h = curl_easy_init();
            if h.is_null() {
                return Err(RequestError::Transport.into());
            }
            let easy = Easy(h);
            curl_easy_setopt_ptr(easy.0, CURLOPT_URL, url_c.as_ptr() as *const c_void);
            curl_easy_setopt_ptr(easy.0, CURLOPT_WRITEFUNCTION, write_cb as *const c_void);
            let mut sink = BodySink::new(max_body);
            curl_easy_setopt_ptr(
                easy.0,
                CURLOPT_WRITEDATA,
                (&mut sink as *mut BodySink) as *mut c_void,
            );
            // No curl call in this module may escape HTTP(S). The public QR fetch is the only one that
            // follows redirects; it is capped, and an HTTPS start may never downgrade to plaintext.
            require_setopt!(
                curl_easy_setopt_long(easy.0, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS),
                "CURLOPT_PROTOCOLS"
            );
            require_setopt!(
                curl_easy_setopt_long(easy.0, CURLOPT_FOLLOWLOCATION, follow_redirects as c_long),
                "CURLOPT_FOLLOWLOCATION"
            );
            if follow_redirects {
                require_setopt!(
                    curl_easy_setopt_long(easy.0, CURLOPT_MAXREDIRS, PUBLIC_MAX_REDIRECTS),
                    "CURLOPT_MAXREDIRS"
                );
                require_setopt!(
                    curl_easy_setopt_long(
                        easy.0,
                        CURLOPT_REDIR_PROTOCOLS,
                        allowed_redirect_protocols(url.as_bytes()),
                    ),
                    "CURLOPT_REDIR_PROTOCOLS"
                );
            }
            require_setopt!(
                curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 1 as c_long),
                "CURLOPT_SSL_VERIFYPEER"
            );
            require_setopt!(
                curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 2 as c_long),
                "CURLOPT_SSL_VERIFYHOST"
            );
            // A pinned request replaces CA verification with key pinning — see
            // [`CURLOPT_PINNEDPUBLICKEY`]. Written AFTER the two defaults above so the ordinary path is
            // still one unconditional pair of lines that cannot be reached with the wrong value.
            //
            // Every security-relevant option above is fail-closed. Pinning is additionally important:
            // if the option is rejected — an older libcurl, or a TLS backend whose
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
                        return Err(RequestError::Transport.into());
                    }
                }
                TlsCfg::Pinned(p) => {
                    let rc = curl_easy_setopt_ptr(
                        easy.0,
                        CURLOPT_PINNEDPUBLICKEY,
                        p.as_ptr() as *const c_void,
                    );
                    if rc != 0 {
                        crate::log(&format!("net: this libcurl refuses CURLOPT_PINNEDPUBLICKEY (rc={rc}) — refusing to send unpinned"));
                        return Err(RequestError::Transport.into());
                    }
                    require_setopt!(
                        curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 0 as c_long),
                        "CURLOPT_SSL_VERIFYPEER"
                    );
                    require_setopt!(
                        curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 0 as c_long),
                        "CURLOPT_SSL_VERIFYHOST"
                    );
                }
            }
            // **Key mode** ([`keypin`]): the remembered key in place of the certificate's dates.
            // `keypin::apply` sets the pin FIRST and relaxes `VERIFYPEER` only once libcurl accepted
            // it, so a refusal here leaves this handle exactly as strict as it was — and the request
            // falls back to the strict failure it already had (or, if none, to a strict attempt).
            if let Some(pin) = &pin_c {
                if let Err(rc) = keypin::apply(easy.0, pin) {
                    crate::log(&format!(
                        "net: this libcurl refuses the key-mode options (rc={rc}) — the request stays strict"
                    ));
                    return Ok(None);
                }
            }
            // The peer's chain, kept for [`peer_leaf_pin`]. A libcurl that refuses the option simply
            // has no chain to give: the request still goes, and answers without a pin.
            let read_peer_pin = read_peer_pin
                && curl_easy_setopt_long(easy.0, CURLOPT_CERTINFO, 1 as c_long) == 0;
            curl_easy_setopt_long(easy.0, CURLOPT_NOSIGNAL, 1 as c_long);
            curl_easy_setopt_long(easy.0, CURLOPT_CONNECTTIMEOUT, t.connect_s);
            if t.total_ms > 0 {
                curl_easy_setopt_long(easy.0, CURLOPT_TIMEOUT_MS, t.total_ms);
            } else {
                curl_easy_setopt_long(easy.0, CURLOPT_TIMEOUT, t.total_s);
            }
            curl_easy_setopt_long(easy.0, CURLOPT_LOW_SPEED_LIMIT, t.low_speed_bps);
            curl_easy_setopt_long(easy.0, CURLOPT_LOW_SPEED_TIME, t.low_speed_s);
            curl_easy_setopt_ptr(easy.0, CURLOPT_USERAGENT, ua.as_ptr() as *const c_void);

            // request headers — keep the CStrings alive until after perform.
            let mut slist = HeaderList(ptr::null_mut());
            for c in &hdr_owned {
                let next = curl_slist_append(slist.0, c.as_ptr());
                if next.is_null() {
                    return Err(RequestError::Transport.into());
                }
                slist.0 = next;
            }
            if !slist.0.is_null() {
                curl_easy_setopt_ptr(easy.0, CURLOPT_HTTPHEADER, slist.0 as *const c_void);
            }
            // The resolve pin. Its list is a second `HeaderList` local for the same lifetime reason as
            // the first: curl keeps the pointer until the transfer ends. NOT `require_setopt!` — a
            // libcurl that answers `CURLE_UNKNOWN_OPTION` here has simply not got the option, and the
            // right outcome is today's DNS path, logged once; every OTHER refusal still cancels
            // (`resolve::note_setopt` decides).
            let mut resolve_list = HeaderList(ptr::null_mut());
            if let Some(r) = &resolve_c {
                let l = curl_slist_append(ptr::null_mut(), r.as_ptr());
                if l.is_null() {
                    return Err(RequestError::Transport.into());
                }
                resolve_list.0 = l;
                let rc = curl_easy_setopt_ptr(easy.0, CURLOPT_RESOLVE, l as *const c_void);
                if resolve::note_setopt(rc).is_err() {
                    return Err(RequestError::Transport.into());
                }
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
                    curl_easy_setopt_ptr(
                        easy.0,
                        CURLOPT_CUSTOMREQUEST,
                        verb_c.as_ptr() as *const c_void,
                    );
                }
            } else if verb != "GET" {
                curl_easy_setopt_ptr(
                    easy.0,
                    CURLOPT_CUSTOMREQUEST,
                    verb_c.as_ptr() as *const c_void,
                );
            }

            let mut rc = curl_easy_perform(easy.0);
            let mut code: c_long = 0;
            let info_rc = curl_easy_getinfo_long(easy.0, CURLINFO_RESPONSE_CODE, &mut code as *mut c_long);
            // Key mode asked libcurl to enforce the pin; a second, independent look at the key the
            // peer really presented is cheap here and makes a backend that accepted the option and
            // never enforced it fail closed. Only a completed handshake has a key to look at.
            if let keypin::Mode::Key { pin, .. } = mode {
                if (rc == 0 || (info_rc == 0 && code != 0)) && !keypin::confirm(easy.0, pin) {
                    rc = keypin::PIN_MISMATCH;
                    code = 0;
                }
            }

            if sink.overflowed {
                crate::log(&format!(
                    "net: response exceeded {} byte body limit",
                    max_body.unwrap_or(0)
                ));
            }
            let verify = if rc == keypin::TLS_VERIFY_FAILED { verify_result(easy.0) } else { None };
            // NAMED, not just counted — logged by the caller once the request's last attempt is
            // known, so a strict failure that key mode is about to answer costs no line here.
            // Everything here rides the TELEVISION's curl and therefore its OpenSSL and its CA store —
            // the library webosbrew's caniuse data singles out as the one that varies most across
            // firmwares. Collapsing every failure to None made a stale CA bundle on a set nobody here
            // owns indistinguishable from being offline: the QR sign-in simply never completes. These
            // are the ones that mean something different from "the network is down". 60 and 51 are
            // explained by the verify result ([`tls_verify_why`]), not blamed on the CA store: a wrong
            // clock fails them too.
            let why = (rc != 0 && !sink.overflowed).then(|| {
                tls_failure_reason(easy.0, rc).unwrap_or_else(|| match rc {
                    35 => "TLS handshake failed (protocol too new for this firmware?)",
                    77 => "CA bundle could not be read",
                    6 => "could not resolve host",
                    28 => "timed out",
                    90 => "certificate pin did not match (stale lab session?)",
                    _ => "transport error",
                }.to_owned())
            });
            let peer_pin = if read_peer_pin && rc == 0 { peer_leaf_pin(easy.0) } else { None };
            Ok(Some(Attempt { rc, info_rc, code, sink, verify, why, peer_pin }))
        }
    };

    // The caller's whole-request budget is spent ONCE across the attempts: a key-mode retry gets
    // what the failed strict handshake left, never a fresh full budget ([`remaining_budget`]).
    let started = std::time::Instant::now();
    let mut budget = t;
    let (done, final_mode) = loop {
        let Some(done) = attempt(&mode, budget)? else {
            // libcurl would not be put in key mode. Never relax anything: the request fails as it
            // would have (the strict failure held), or, when key mode was the latched START, goes
            // strict after all.
            if let Some(key) = &key {
                keypin::unlatch(key);
            }
            match held.take() {
                Some(strict) => break (strict, keypin::Mode::Strict),
                None => {
                    mode = keypin::Mode::Strict;
                    continue;
                }
            }
        };
        if let (keypin::Mode::Strict, Some(key)) = (&mode, &key) {
            if let Some(next) = keypin::after_strict_failure(key, done.rc, done.verify) {
                match remaining_budget(t, started.elapsed()) {
                    Some(left) => {
                        budget = left;
                        held = Some(done);
                        mode = next;
                        continue;
                    }
                    // Nothing is left to spend on the retry: the request fails as the strict
                    // attempt did, inside the time the caller allowed.
                    None => break (done, mode),
                }
            } else if done.rc != 0 {
                // No key mode to retry in: what this failure says about the host is a fact the app
                // can tell a viewer about (a date failure with no key), or ends one (anything else).
                keypin::strict_failure(key, done.rc, done.verify);
            }
        }
        break (done, mode);
    };
    let Attempt { rc, info_rc, code, sink, why, peer_pin, .. } = done;
    // What the attempt that decided the request says about the host's mode.
    let established = rc == 0 || (info_rc == 0 && code != 0);
    if let Some(key) = &key {
        match &final_mode {
            keypin::Mode::Strict if established => keypin::strict_established(key),
            keypin::Mode::Key { .. } if rc == keypin::PIN_MISMATCH => keypin::key_refused(key),
            keypin::Mode::Key { pin, verify } if established => keypin::key_established(key, pin, *verify),
            _ => {}
        }
    }
    if let Some(why) = why {
        // A key-mode pin mismatch has its own line ([`keypin::key_refused`]); this one would call
        // it a stale lab session.
        if !(matches!(final_mode, keypin::Mode::Key { .. }) && rc == keypin::PIN_MISMATCH) {
            crate::log(&format!("net: curl rc={rc} — {why}"));
        }
    }
    finish_response(rc, info_rc, code, follow_redirects, max_body, sink)
        .map(|resp| Resp { peer_pin, ..resp })
}

/// What is left of `t`'s whole-request deadline after `elapsed`, as a millisecond deadline, for a
/// key-mode retry that follows a failed strict attempt: `None` when nothing is left (the caller
/// returns the strict failure), `t` itself when the request has no whole-request deadline
/// (`total_ms` and `total_s` both zero, the low-speed pair being the only bound).
fn remaining_budget(t: Timeouts, elapsed: std::time::Duration) -> Option<Timeouts> {
    let total_ms = match (t.total_ms, t.total_s) {
        (ms, _) if ms > 0 => i128::from(ms),
        (_, s) if s > 0 => i128::from(s) * 1000,
        _ => return Some(t),
    };
    let left = total_ms - elapsed.as_millis() as i128;
    // A millisecond deadline of 0 would mean "none" to libcurl; a retry with under 1 ms is not one.
    (left >= 1).then(|| Timeouts { total_ms: left as c_long, ..t })
}

/// What one pass of [`request_tls_evidence`]'s attempt closure leaves behind. Plain data: the easy
/// handle and every list it referenced are gone by the time this exists.
struct Attempt {
    rc: c_int,
    info_rc: c_int,
    code: c_long,
    sink: BodySink,
    /// [`CURLINFO_SSL_VERIFYRESULT`], read only for the code that carries one (60).
    verify: Option<c_long>,
    /// The log phrase for a failed transfer, kept until the request's last attempt is known.
    why: Option<String>,
    peer_pin: Option<String>,
}

/// May this request read the peer's public key, and may [`keypin`] consider it (the one rule for
/// both): only over https, only when the peer was
/// verified against a trust store (`Tls::Ca`, or the test `Tls::CaBundle`), and only when no
/// redirect can have moved the connection to another peer. [`Tls::Pinned`] is the lab receiver,
/// which turns verification OFF — a key learned from a connection nobody authenticated would be
/// a stranger's key remembered as the server's.
fn peer_pin_wanted(url: &str, tls: &Tls<'_>, follow_redirects: bool) -> bool {
    let https = url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"));
    https && !follow_redirects && matches!(tls, Tls::Ca | Tls::CaBundle(_))
}

/// Read-only view of libcurl's `struct curl_certinfo`: the number of certificates, then one
/// `struct curl_slist *` per certificate. Never constructed by us except in tests.
#[repr(C)]
struct CurlCertInfo {
    num_of_certs: c_int,
    certinfo: *const *const CurlStringNode,
}

/// Read-only view of `struct curl_slist` — a NUL-terminated string and the next node. The public
/// [`curl_slist`] alias stays an opaque `c_void`; this is the one place that looks inside one.
#[repr(C)]
struct CurlStringNode {
    data: *const c_char,
    next: *const CurlStringNode,
}

/// Most nodes read from one certificate's list. libcurl's OpenSSL backend writes a dozen
/// "Name:value" lines per certificate; the bound is only here so a malformed list cannot loop.
const CERTINFO_MAX_NODES: usize = 64;
/// Longest `Cert:` PEM accepted: far above any real leaf, far below an allocation worth worrying
/// about.
const CERTINFO_MAX_PEM: usize = 16 * 1024;

/// The pin of the peer's LEAF certificate from a finished transfer, or `None` when libcurl kept
/// no chain (backend without `CERTINFO`, option refused) or what it kept does not parse.
fn peer_leaf_pin(easy: *mut CURL) -> Option<String> {
    if easy.is_null() {
        return None;
    }
    let mut info: *const c_void = ptr::null();
    // SAFETY: `easy` is a live handle the caller owns and has not cleaned up; the out pointer is a
    // local pointer, which is what `CURLINFO_CERTINFO` writes. The structure it returns belongs to
    // the handle and is only read here, before the handle is dropped.
    let got = unsafe {
        curl_easy_getinfo_ptr(easy, CURLINFO_CERTINFO, &mut info as *mut *const c_void)
    } == 0;
    // SAFETY: a non-null answer is libcurl's own `struct curl_certinfo`, valid until cleanup.
    got.then(|| unsafe { leaf_pin_of_certinfo(info as *const CurlCertInfo) }).flatten()
}

/// The pin of certificate index 0 (the peer's own, ahead of its issuers) in `info`.
///
/// # Safety
/// `info` is null or points to a well-formed `curl_certinfo` whose lists and strings stay valid
/// for the call. Every pointer is null-checked and both walks are bounded; the PEM text itself is
/// untrusted network input and goes through [`crate::spki::pin_from_pem`], which bounds-checks it.
unsafe fn leaf_pin_of_certinfo(info: *const CurlCertInfo) -> Option<String> {
    let info = unsafe { info.as_ref() }?;
    if info.num_of_certs < 1 || info.certinfo.is_null() {
        return None;
    }
    let mut node = unsafe { *info.certinfo };
    for _ in 0..CERTINFO_MAX_NODES {
        let n = unsafe { node.as_ref() }?;
        if !n.data.is_null() {
            let line = unsafe { std::ffi::CStr::from_ptr(n.data) }.to_bytes();
            if let Some(pem) = line.strip_prefix(b"Cert:") {
                if pem.len() > CERTINFO_MAX_PEM {
                    return None;
                }
                return crate::spki::pin_from_pem(std::str::from_utf8(pem).ok()?);
            }
        }
        node = n.next;
    }
    None
}

/// Civil `(year, month, day)` of a Unix time in UTC — Howard Hinnant's `civil_from_days`, with
/// Unix day zero shifted to the civil epoch. Pure, so a wrong-clock log line and the tests that mint
/// a certificate relative to "now" share one calendar.
pub(crate) fn civil_date(unix_secs: i64) -> (i64, u32, u32) {
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    ((yoe + era * 400) + i64::from(month <= 2), month, day)
}

/// The year this device believes it is, from its wall clock. `None` only if the clock reads
/// before 1970, which a log line cannot usefully say anything about.
fn wall_clock_year() -> Option<i64> {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(civil_date(secs as i64).0)
}

/// The log sentence for a failed TLS peer verification, or `None` for a code that is not one.
///
/// `rc` is the `CURLcode`; `verify` is [`CURLINFO_SSL_VERIFYRESULT`] read off the same handle
/// (`None` when libcurl would not say); `year` is [`wall_clock_year`]. Libcurl's 60 is one code
/// for every reason a chain failed, so the sentence comes from the verify result and never blames
/// the CA store by default. A television cold-booted without internet has a wrong clock (GitHub discussion #351), which
/// makes a perfectly valid certificate "expired" or "not yet valid" — and the year the device
/// believes it is the evidence that confirms it. Closed vocabulary: no host, URL or certificate
/// field is ever interpolated, only the numbers.
///
/// 51 is `CURLE_PEER_FAILED_VERIFICATION` on the television's 7.53.1: the name check, which
/// OpenSSL records outside the verify result, so it is decided from the code alone. libcurl 7.62+
/// retired 51 and reports a name mismatch as 60 with verify result 0 (the macOS host and newer
/// firmwares), so that case says so rather than hide the likeliest cause.
pub(crate) fn tls_verify_why(rc: c_int, verify: Option<c_long>, year: Option<i64>) -> Option<String> {
    let clock = |what: &str| match year {
        Some(y) => format!("{what} — the device clock may be wrong (it believes the year is {y})"),
        None => format!("{what} — the device clock may be wrong"),
    };
    match (rc, verify) {
        (51, _) => Some("certificate name does not match host".to_owned()),
        (60, Some(9)) => Some(clock("peer certificate is not yet valid")),
        (60, Some(10)) => Some(clock("peer certificate has expired")),
        (60, Some(18 | 19)) => Some("peer certificate is self-signed".to_owned()),
        (60, Some(20 | 21)) => {
            Some("peer certificate issuer could not be verified (CA store too old?)".to_owned())
        }
        (60, Some(n)) if n != 0 => Some(format!("peer certificate could not be verified (X509 verify result {n})")),
        (60, _) => Some(
            "peer certificate could not be verified (no X509 verify result; on libcurl 7.62+ this is how a certificate name mismatch reads)"
                .to_owned(),
        ),
        _ => None,
    }
}

/// [`tls_verify_why`] for a handle whose transfer just failed with `rc`: reads the verify result
/// only for the two codes that can carry one, so every other failure costs no extra call.
/// Shared by both TLS clients to the PMS — this module's control plane and `curlio`'s media
/// plane — because both ride the same firmware OpenSSL and the same wrong clock.
pub(crate) fn tls_failure_reason(easy: *mut CURL, rc: c_int) -> Option<String> {
    if !matches!(rc, 51 | 60) {
        return None;
    }
    tls_verify_why(rc, verify_result(easy), wall_clock_year())
}

/// [`CURLINFO_SSL_VERIFYRESULT`] off a handle whose transfer just ended, `None` when libcurl would
/// not say. The raw number key mode's trigger reads ([`keypin::after_strict_failure`]).
pub(crate) fn verify_result(easy: *mut CURL) -> Option<c_long> {
    let mut verify: c_long = 0;
    // SAFETY: `easy` is a live handle the caller owns and has not yet cleaned up; the out pointer
    // is a local `long`, which is what `CURLINFO_SSL_VERIFYRESULT` writes.
    let got = !easy.is_null()
        && unsafe { curl_easy_getinfo_long(easy, CURLINFO_SSL_VERIFYRESULT, &mut verify as *mut c_long) } == 0;
    got.then_some(verify)
}

/// Option-projected blocking HTTPS GET on the [`API`] deadlines.
#[allow(dead_code)] // Preserve the Option compatibility API; account now opts into evidence.
pub fn https_get(url: &str, headers: &[String]) -> Option<Resp> {
    request(url, headers, "GET", None, API, false, None, None)
}
/// Blocking HTTPS POST (`body` may be empty) on the [`API`] deadlines.
#[allow(dead_code)] // Preserve the Option compatibility API; account now opts into evidence.
pub fn https_post(url: &str, headers: &[String], body: &[u8]) -> Option<Resp> {
    request(url, headers, "POST", Some(body), API, false, None, None)
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
    request_tls(
        url,
        headers,
        "POST",
        Some(body),
        t,
        false,
        Some(4096),
        Tls::Pinned(pin),
    )
}

/// Blocking **CA-verified, unpinned** HTTPS POST to a third-party endpoint — the telemetry sinks.
///
/// # Why this exists, given that [`https_post`] is already CA-verified and unpinned
///
/// The plan this was built to called for a new request mode on the grounds that [`post_pinned`]
/// sets `SSL_VERIFYPEER=0`, so "Sentry and PostHog need the opposite". They do — and `request` has
/// been that opposite since it was written: `VERIFYPEER=1`/`VERIFYHOST=2` by default, lowered in
/// exactly two places: the lab's pinning branch and [`keypin`]'s date-only retry, which needs a
/// remembered key for the host and so never applies to a telemetry sink. The premise was wrong,
/// and the mode it asked for already existed. Recording that rather than quietly building it,
/// because "add a mode that is already the default" is the kind of finding that otherwise gets
/// rediscovered.
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
    request_tls(
        url,
        headers,
        "POST",
        Some(body),
        t,
        false,
        Some(TELEMETRY_MAX_REPLY),
        tls,
    )
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
    request(url, &[], "GET", None, API, true, None, None)
}

/// The `nowan` gate shared by the three resolver doors ([`request_tls_result`],
/// [`crate::curlio`] and [`crate::stream`]): `true` when `/tmp/plxnative-nowan` is armed and
/// `host` is a NAME rather than a literal, i.e. when a dead resolver would have refused it. The
/// `slow` variant first spends `connect_s`, the budget a worker would have lost waiting on that
/// resolver. `false` without the trigger, and at compile time without `devtriggers`.
pub(crate) fn refuse_name(host: &str, connect_s: c_long) -> bool {
    let Some(nw) = crate::dev::no_wan() else {
        return false;
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    if nw.slow {
        std::thread::sleep(std::time::Duration::from_secs(connect_s.max(0) as u64));
    }
    // `host=`, not a bare `{host}` interpolation, so `diag::scrub::scrub_local`'s host clause
    // catches it — a private hostname reaching this line unredacted is the exact device leak
    // `stream.rs`'s DNS-failure line had.
    crate::log(&format!("net: nowan — refused name host={host}"));
    true
}

/// **Resolve pins — the DNS answers this process already knows.**
///
/// A [`ResolvePin`] says "this `plex.direct` name IS this address", validated by
/// [`ResolvePin::for_origin`] so that it is a pure function of the hostname (its doc has the
/// rule). The control plane carries the pin on each `Client` and passes it into
/// [`request_result`] explicitly. The media plane cannot: `ff::demux` receives a URL string and
/// `curlio::CurlSource` builds a fresh easy handle on every open and seek, so it asks THIS table
/// by the URL's host and port instead.
///
/// **Append-only, process-lifetime, never replaced** (the remembered KEYS, [`keypin`], beside it
/// are the opposite: replaced on a change, emptied at sign-out). A registry slot is re-pointed by publishing
/// a new `Client` over a leaked old one, and a worker mid-stream keeps the old reference — so a
/// table that removed or rewrote an entry on re-point could change the resolution of a route a
/// demuxer already captured. Because a valid pin cannot become wrong, nothing here ever needs to
/// be taken back: a re-point that lands on a new host appends, and the old entry stays true.
/// The table holds one entry per server address this process has ever pinned — a handful.
pub(crate) mod resolve {
    use super::{c_int, Mutex, Ordering, CURLE_UNKNOWN_OPTION};
    use crate::plex::ResolvePin;

    static PINS: Mutex<Vec<ResolvePin>> = Mutex::new(Vec::new());

    /// Record a pin. `true` when it was new, `false` when the same host and port were already
    /// pinned (to the same address, by construction).
    pub(crate) fn add(pin: &ResolvePin) -> bool {
        let mut pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
        if pins
            .iter()
            .any(|p| p.host() == pin.host() && p.port() == pin.port())
        {
            return false;
        }
        pins.push(pin.clone());
        true
    }

    /// The pin for `host:port`, if this process has recorded one.
    pub(crate) fn lookup(host: &str, port: i32) -> Option<ResolvePin> {
        PINS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|p| p.host() == host && p.port() == port)
            .cloned()
    }

    /// The `CURLOPT_RESOLVE` entry for a pin, in the syntax the bound libcurl parses.
    pub(crate) fn entry_of(pin: &ResolvePin) -> String {
        pin.entry(super::curl_version_num())
    }

    /// [`lookup`] then [`entry_of`] — what the media plane asks for a URL it is about to open.
    pub(crate) fn entry_for(host: &str, port: i32) -> Option<String> {
        lookup(host, port).map(|p| entry_of(&p))
    }

    /// Grade a `curl_easy_setopt(CURLOPT_RESOLVE)` result. `Ok` means the request may go on:
    /// either the pin took, or this libcurl has no such option (`CURLE_UNKNOWN_OPTION`, 48) and
    /// the name resolves through DNS as it did before the pin existed — logged once per process
    /// so the fact is in the log from a set nobody here owns. Any OTHER refusal is `Err`: a
    /// malformed entry or a broken handle is not a reason to quietly send the request elsewhere.
    /// Under `nowan` even 48 is `Err`, because DNS is deliberately unavailable there.
    pub(crate) fn note_setopt(rc: c_int) -> Result<(), ()> {
        if rc == 0 {
            return Ok(());
        }
        if rc == CURLE_UNKNOWN_OPTION {
            static REPORTED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !REPORTED.swap(true, Ordering::Relaxed) {
                crate::log(
                    "net: resolve pin not applied (rc=48, this libcurl has no CURLOPT_RESOLVE); \
                     names resolve through DNS",
                );
            }
            return if crate::dev::no_wan().is_some() { Err(()) } else { Ok(()) };
        }
        crate::log(&format!("net: resolve pin refused (rc={rc}); request cancelled"));
        Err(())
    }

    /// Tests share one process-global table; this empties it between them.
    #[cfg(test)]
    pub(crate) fn clear() {
        PINS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// **Key mode — recognising a server by its remembered key when the certificate fails its date check.**
///
/// A television has no real-time clock. Cold-booted with no internet its clock is wrong, so the
/// household server's perfectly valid `*.plex.direct` certificate fails libcurl's date check
/// ([`TLS_VERIFY_FAILED`], 60, with `CURLINFO_SSL_VERIFYRESULT` 9 "not yet valid" or 10
/// "expired") and the server one hop away is unreachable (issue #378). While online the identity
/// probe remembers each server's leaf public key from a connection that passed STRICT
/// verification ([`Resp::peer_pin`], `plex::session::learn_server_key`); this module is where that
/// key is used.
///
/// **What is relaxed, when, and what still holds.** A request retries in key mode only when ALL of
/// these hold ([`after_strict_failure`]): it ran in strict mode (a CA store, never [`Tls::Pinned`],
/// never plaintext); libcurl said 60; the verify result was 9 or 10; and the table below holds a
/// key for that `host:port`. Key mode sets `CURLOPT_PINNEDPUBLICKEY` to the remembered key and
/// turns `CURLOPT_SSL_VERIFYPEER` off — which drops the chain and date check: `CURLOPT_SSL_VERIFYHOST`
/// stays 2 (the certificate must still be issued for the name in the URL), the key must still match
/// (rc 90 otherwise), and [`apply`] makes the relaxation impossible without an accepted pin. A
/// failure whose verify result is anything else (an untrusted issuer reported first, 18-21; a name
/// mismatch) is refused exactly as before even when a key is held.
///
/// **What the trigger does NOT prove, stated plainly.** The security of key mode rests on the key
/// pin plus the name check, not on the date being the only defect. (1) Verify result 9/10 does not
/// show that the rest of the chain would have passed: OpenSSL stops at the first error it meets
/// walking the chain, so a certificate that is expired AND untrusted reads as 10 too. (2) Key mode
/// also engages for a genuinely expired certificate on a correct clock, because the trigger is the
/// verify result and not a judgement about the clock. (3) The remembered key has no expiry: it lasts
/// as long as the session that holds it.
///
/// **Two tables, one lifetime rule.** The key table is keyed by lowercase `host:port`, beside
/// [`resolve`]'s but with REPLACE semantics: it is a projection of the session's remembered keys
/// (`plex::session::project_server_keys`), recomputed whenever the session changes, and empty after
/// sign-out. The latch records which `host:port`s are currently being served in key mode: after a
/// fallback succeeds, later requests go straight to key mode instead of failing a strict handshake
/// first, for [`LATCH`] of monotonic time, after which strict is tried again. A strict success, a
/// pin change for the host and a key-mode pin mismatch each clear it.
///
/// A request that ran in key mode never reports a [`Resp::peer_pin`]: that handshake was not
/// strictly verified, and a key learned from it would be a stranger's remembered as the server's.
///
/// **Facts, and one toast.** Where each decision is already made this module also publishes what it
/// means, for the app to poll by [`keypin::revision`] (`plex::grant`'s shape): [`keypin::engaged`]
/// (key mode has engaged this run, with the year the device believed at the first time) and
/// [`keypin::blocked_for`] ([`keypin::Blocked::NoKey`]: a date failure and no key held;
/// [`keypin::Blocked::KeyChanged`]: rc 90), per host and asked per machine. A host's fact is its
/// LATEST strict outcome: cleared by its strict or key-mode success or a pin change, and a
/// [`keypin::Blocked::NoKey`] by a later strict failure that is not about the date. `app::clock_notice` turns the first engagement into the one system
/// toast; the log lines remain the trace for everything else.
pub(crate) mod keypin {
    use super::{
        c_int, c_long, curl_easy_setopt_long, curl_easy_setopt_ptr, peer_leaf_pin, tls_verify_why,
        wall_clock_year, CURL, CURLOPT_CERTINFO, CURLOPT_FORBID_REUSE, CURLOPT_FRESH_CONNECT,
        CURLOPT_PINNEDPUBLICKEY, CURLOPT_SSL_VERIFYHOST, CURLOPT_SSL_VERIFYPEER,
    };
    use std::collections::HashMap;
    use std::ffi::{c_void, CStr};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    /// How long a host stays in key mode after a fallback succeeded, on the monotonic clock.
    pub(super) const LATCH: Duration = Duration::from_secs(10 * 60);
    /// libcurl's rc 60: the certificate chain or its dates were refused. Named
    /// `CURLE_SSL_CACERT` below libcurl 7.62 (the television's 7.53.1) and
    /// `CURLE_PEER_FAILED_VERIFICATION` from 7.62, where 51 stops meaning that; the number is what
    /// is stable, so this name is version-neutral. The one definition, shared with `curlio`.
    pub(crate) const TLS_VERIFY_FAILED: c_int = 60;
    /// `CURLE_SSL_PINNEDPUBKEYNOTMATCH`: the server presented a key other than the pinned one.
    pub(crate) const PIN_MISMATCH: c_int = 90;
    /// OpenSSL `X509_V_ERR_CERT_NOT_YET_VALID` and `X509_V_ERR_CERT_HAS_EXPIRED`: the two verify
    /// results that are about the date. They do not prove the date was the only problem.
    const X509_NOT_YET_VALID: c_long = 9;
    const X509_EXPIRED: c_long = 10;

    /// libcurl 60 with a date verify result: the one failure a wrong clock explains.
    fn is_date_failure(rc: c_int, verify: Option<c_long>) -> bool {
        rc == TLS_VERIFY_FAILED && matches!(verify, Some(X509_NOT_YET_VALID | X509_EXPIRED))
    }

    /// How one attempt of a request recognises the peer.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Mode {
        /// The ordinary, fully verified handshake.
        Strict,
        /// The remembered key, in place of the certificate's dates. `verify` is the strict
        /// failure that led here, kept only for the one log line.
        Key { pin: String, verify: Option<c_long> },
    }

    #[derive(Default)]
    struct State {
        /// The session's remembered keys, by `machineIdentifier` (replaced wholesale).
        machine_pins: HashMap<String, String>,
        /// Which `host:port` each machine is served at — recorded where a `ResolvePin` is
        /// installed and read off the stored session.
        bindings: Vec<(String, String)>,
        /// `host:port` to the pin this process would accept in key mode.
        table: HashMap<String, String>,
        /// `host:port` to the pin it is being served under and when that began.
        latched: HashMap<String, (String, Instant)>,
        /// `host:port` to why key mode cannot help it right now ([`Blocked`]).
        blocked: HashMap<String, Blocked>,
        /// `host:port` to the year the device believed when key mode FIRST engaged for it, with
        /// the order engagements happened in. Never cleared by a latch lapse, a success or a pin
        /// change: it is a fact about this app run, not about the host's present state.
        engaged: HashMap<String, (u64, Option<i64>)>,
        /// The next engagement's place in [`State::engaged`]'s order.
        engaged_seq: u64,
    }

    /// **Why key mode cannot help a host** — the facts the app reads to tell a viewer their server
    /// is unreachable for a reason a clock explains. Published where each decision is already made
    /// and never decided a second time.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) enum Blocked {
        /// A strict attempt failed on the certificate's DATES (libcurl 60, verify 9 or 10 — the
        /// date check was the first failure the chain walk met, not necessarily the only defect,
        /// see the module's caveat) and the table holds no key for the host, so there was nothing
        /// to recognise it by.
        NoKey,
        /// The host presented a different key than the remembered one (rc 90). Outranks
        /// [`Blocked::NoKey`] (the variants are in ascending rank): a key was held and was wrong.
        KeyChanged,
    }

    /// Bumped on every change to a fact, so a poller (the app's frame loop) compares one integer
    /// instead of taking the lock. The `plex::grant::revision` shape.
    static REVISION: AtomicU64 = AtomicU64::new(0);

    /// The current revision of everything this module publishes ([`blocked_for`], [`engaged`]).
    pub(crate) fn revision() -> u64 {
        REVISION.load(Ordering::Acquire)
    }

    fn moved() {
        REVISION.fetch_add(1, Ordering::AcqRel);
    }

    /// Record `why` for `key`. A repeat of the same fact is not a change.
    fn set_blocked(st: &mut State, key: &str, why: Blocked) {
        if st.blocked.insert(key.to_owned(), why) != Some(why) {
            moved();
        }
    }

    fn clear_blocked(st: &mut State, key: &str) {
        if st.blocked.remove(key).is_some() {
            moved();
        }
    }

    /// Why key mode cannot help the server `machine_id`, if it cannot: the facts of every
    /// `host:port` bound to that machine (its LAN address and its plex.direct name are two), with
    /// [`Blocked::KeyChanged`] outranking [`Blocked::NoKey`] when both stand. **Asked per machine**:
    /// a read-out is about one server, and another server's expired certificate says nothing about
    /// it. A machine nothing is bound to (and the empty id, which names none) has no fact.
    #[allow(dead_code)] // The Home/Library read-out that reads it is not carried by this maintenance line; tests and `clockfact` use it.
    pub(crate) fn blocked_for(machine_id: &str) -> Option<Blocked> {
        let st = state();
        let bound = st.bindings.iter().filter(|(m, _)| m == machine_id).map(|(_, hp)| hp.as_str());
        // The dev `clockfact` plant is synthetic and belongs to no machine: it answers for all.
        #[cfg(feature = "devtriggers")]
        let bound = bound.chain([PLANTED_KEY]);
        bound
            .filter_map(|hp| st.blocked.get(hp).copied())
            .max()
    }

    /// `Some(year)` once key mode has engaged for any host in this app run, carrying the year the
    /// device believed at the FIRST engagement (`None` inside when it could not say). Survives the
    /// latch lapsing and re-engaging, and a sign-out: it records what happened, and the once-per-run
    /// toast it feeds owns its own flag.
    pub(crate) fn engaged() -> Option<Option<i64>> {
        state().engaged.values().min_by_key(|(seq, _)| *seq).map(|(_, year)| *year)
    }

    /// One host's facts, for tests that key their own host so parallel tests share nothing.
    #[cfg(test)]
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct Facts {
        pub(crate) blocked: Option<Blocked>,
        pub(crate) engaged: Option<Option<i64>>,
    }

    #[cfg(test)]
    pub(crate) fn fact_for(key: &str) -> Facts {
        let st = state();
        Facts { blocked: st.blocked.get(key).copied(), engaged: st.engaged.get(key).map(|(_, year)| *year) }
    }

    fn state() -> MutexGuard<'static, State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(State::default())).lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The table's key: lowercase `host:port`, the host unbracketed (`Origin::host`).
    pub(crate) fn key_of(host: &str, port: i32) -> String {
        format!("{}:{port}", host.to_ascii_lowercase())
    }

    /// [`key_of`] a URL's origin, or `None` for anything but `https` (key mode is a TLS fallback).
    /// The same reading of a URL the media plane's resolve lookup uses, so the two tables agree.
    pub(crate) fn key_of_url(url: &str) -> Option<String> {
        url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")).then(|| {
            let (origin, _) = crate::plex::origin::split(url);
            key_of(origin.host(), origin.port())
        })
    }

    /// Recompute the table from the two inputs. Only `host:port`s that have a binding are touched,
    /// so a pin and the latch that stood on it cannot outlive the pin's removal or change.
    fn recompute(st: &mut State) {
        let mut next: HashMap<String, String> = HashMap::new();
        for (machine, hp) in &st.bindings {
            if let Some(pin) = st.machine_pins.get(machine) {
                next.insert(hp.clone(), pin.clone());
            }
        }
        let bound: Vec<String> = st.bindings.iter().map(|(_, hp)| hp.clone()).collect();
        for hp in bound {
            // A pin that changed, arrived or went changes what key mode can do for the host, so
            // what was published about it is over. A projection that changes nothing clears nothing.
            if st.table.get(&hp) != next.get(&hp) {
                clear_blocked(st, &hp);
            }
            match next.get(&hp) {
                Some(pin) => {
                    if st.table.get(&hp) != Some(pin) {
                        st.latched.remove(&hp);
                    }
                    st.table.insert(hp, pin.clone());
                }
                None => {
                    st.table.remove(&hp);
                    st.latched.remove(&hp);
                }
            }
        }
    }

    fn add_binding(st: &mut State, machine_id: &str, host: &str, port: i32) {
        if machine_id.is_empty() {
            return;
        }
        let hp = key_of(host, port);
        if !st.bindings.iter().any(|(m, h)| m == machine_id && *h == hp) {
            st.bindings.push((machine_id.to_owned(), hp));
        }
    }

    /// A server's `ResolvePin` was just installed in [`super::resolve`] for `machine_id`: remember
    /// that its key, if the session holds one, belongs to this `host:port`.
    pub(crate) fn bind(machine_id: &str, host: &str, port: i32) {
        let mut st = state();
        add_binding(&mut st, machine_id, host, port);
        recompute(&mut st);
    }

    /// The session → table projection, in one place (`plex::session::project_server_keys` is its
    /// only production caller). `machine_pins` REPLACES what was remembered; `stored` are the
    /// `(machine, host, port)` of every stored server that has a `ResolvePin`, bound here so a
    /// boot that has not registered anything yet is already covered. `signed_out` is the CALLER's
    /// statement that the session is over (signed out, cleared or revoked) — never inferred from
    /// the inputs being empty, because a live session that has learned no key and stores no resolve
    /// pin looks exactly like that, and this runs on every session read and write.
    pub(crate) fn project(machine_pins: Vec<(String, String)>, stored: &[(String, String, i32)], signed_out: bool) {
        let mut st = state();
        st.machine_pins = machine_pins.into_iter().collect();
        for (machine, host, port) in stored {
            add_binding(&mut st, machine, host, *port);
        }
        recompute(&mut st);
        if signed_out {
            // Sign-out: the table is empty and so is the session, so nothing published about a
            // server this session knew still stands. A host that never held a key is not covered
            // by the pin change above, hence this. [`State::engaged`] stays: it is history.
            let bound: Vec<String> = st.bindings.iter().map(|(_, hp)| hp.clone()).collect();
            for hp in bound {
                clear_blocked(&mut st, &hp);
            }
        }
    }

    /// Put one machine's key in the table with no session behind it. **Not a production path**:
    /// the session projection ([`project`]) is the only production source of a key, so a key can
    /// neither outlive a sign-out nor be reverted by a projection that lands after it. The dev
    /// `tls-selftest` trigger needs exactly this (it holds no session) and restates its key before
    /// every round for the reason that [`project`] replaces it.
    #[cfg(feature = "devtriggers")]
    pub(crate) fn note_machine_pin(machine_id: &str, pin: &str) {
        let mut st = state();
        st.machine_pins.insert(machine_id.to_owned(), pin.to_owned());
        recompute(&mut st);
    }

    /// What the dev `clockfact` trigger plants.
    #[cfg(feature = "devtriggers")]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Planted {
        /// [`Blocked`], as if a bound server had just failed that way.
        Blocked(Blocked),
        /// Key mode engaged, the device believing `year`.
        Engaged(Option<i64>),
    }

    /// The synthetic host a planted fact is filed under. Never bound, so no projection, pin change
    /// or sign-out (all of which only touch BOUND hosts) can clear it, and no request can reach it.
    #[cfg(feature = "devtriggers")]
    pub(crate) const PLANTED_KEY: &str = "clockfact.invalid:0";

    /// **Not a production path.** Publish a fact for the simulator and the television with no
    /// request behind it, so a read-out that reads [`blocked_for`] / [`engaged`] can be looked at. It
    /// is SYNTHETIC: it bypasses the bound-hosts rule of [`strict_failure`] on purpose, files
    /// the fact under [`PLANTED_KEY`], and states itself in the log.
    #[cfg(feature = "devtriggers")]
    pub(crate) fn plant(fact: Planted) {
        let mut st = state();
        match fact {
            Planted::Blocked(why) => set_blocked(&mut st, PLANTED_KEY, why),
            Planted::Engaged(year) => {
                let seq = st.engaged_seq;
                st.engaged_seq += 1;
                st.engaged.insert(PLANTED_KEY.to_owned(), (seq, year));
                moved();
            }
        }
        drop(st);
        crate::log("net: a synthetic key-mode fact was planted by the clockfact dev trigger");
    }

    /// The mode a request starts in: key mode while the host is latched and the latch is live,
    /// strict otherwise. A lapsed or stale latch is dropped here.
    pub(crate) fn begin(key: &str) -> Mode {
        begin_at(key, Instant::now())
    }

    /// [`begin`] at `now`: the latch is live while less than [`LATCH`] has passed since it began.
    pub(super) fn begin_at(key: &str, now: Instant) -> Mode {
        let mut st = state();
        if let Some((pin, since)) = st.latched.get(key).cloned() {
            if st.table.get(key) == Some(&pin) && now.saturating_duration_since(since) < LATCH {
                return Mode::Key { pin, verify: None };
            }
            st.latched.remove(key);
        }
        Mode::Strict
    }

    /// **The trigger, and only the trigger.** A strict attempt failed with `rc` and `verify` (the
    /// handle's `CURLINFO_SSL_VERIFYRESULT`): key mode when libcurl said 60 with a date verify
    /// result and the table holds a key for `key`.
    pub(crate) fn after_strict_failure(key: &str, rc: c_int, verify: Option<c_long>) -> Option<Mode> {
        if !is_date_failure(rc, verify) {
            return None;
        }
        let pin = state().table.get(key)?.clone();
        Some(Mode::Key { pin, verify })
    }

    /// A strict handshake completed: whatever latch stood for `key` is over, and so is whatever
    /// was published about that host (and only that host: a plex.tv success says nothing of a PMS).
    pub(crate) fn strict_established(key: &str) {
        let mut st = state();
        st.latched.remove(key);
        clear_blocked(&mut st, key);
    }

    /// Key mode is not being used for `key` after all (libcurl would not be put in it): its latch is
    /// over, and nothing else is claimed — this is not a strict success.
    pub(crate) fn unlatch(key: &str) {
        state().latched.remove(key);
    }

    /// A strict attempt failed and [`after_strict_failure`] found no key mode to retry in: record
    /// what that says about `key`. **The fact is the host's LATEST strict outcome**, so this either
    /// publishes or ends [`Blocked::NoKey`]:
    ///
    /// * the failure was about the DATE (libcurl 60, verify 9 or 10), `key` is a bound media server
    ///   and the table holds no key for it — exactly [`after_strict_failure`]'s trigger minus the
    ///   key — publishes it. A failure it would have answered with a key, a host that is no media
    ///   server (plex.tv) and a host that already has a fact publish nothing new;
    /// * any other failure (a refused connection, a timeout, an untrusted issuer, a name mismatch)
    ///   ends it: the viewer fixed the clock and the server is unreachable for another reason, and
    ///   *Try again* must not go on blaming a clock that is right.
    ///
    /// A [`Blocked::KeyChanged`] is NOT ended here: the host presented a different key than the
    /// remembered one, and a later failure that says nothing about the key does not make it the
    /// remembered one again. A pin change, or a success, does (`recompute`, [`strict_established`],
    /// [`key_established`]). Quiet: the request's own failure log line already says it. Only ever
    /// WRITES a fact; no TLS decision reads it.
    pub(crate) fn strict_failure(key: &str, rc: c_int, verify: Option<c_long>) {
        let mut st = state();
        if !is_date_failure(rc, verify) {
            if st.blocked.get(key) == Some(&Blocked::NoKey) {
                clear_blocked(&mut st, key);
            }
            return;
        }
        // Only a registered media server: `bindings` is every `host:port` `servers::register_lazy`
        // or the stored-session projection ties to a machine, whether or not it holds a key yet,
        // so a server that never learned one is covered. plex.tv (and any other host) fails its
        // date check on a wrong clock too, but key mode can never serve it and nothing clears the
        // fact except its own success, so it would colour a read-out it did not cause.
        if st.table.contains_key(key) || !st.bindings.iter().any(|(_, hp)| hp == key) {
            return;
        }
        // A changed key outranks a missing one, and a host that has just lost its key cleared the
        // changed-key fact with it (`recompute`), so only an absent fact is filled here.
        if !st.blocked.contains_key(key) {
            set_blocked(&mut st, key, Blocked::NoKey);
        }
    }

    /// The media stack's rc 90 in key mode: the host presented a different key. Ends key mode for
    /// the host exactly as [`unlatch`] does, and publishes [`Blocked::KeyChanged`]; the log line is
    /// `curlio`'s own (`curl_why(90)`), so this adds none.
    pub(crate) fn key_changed(key: &str) {
        let mut st = state();
        st.latched.remove(key);
        set_blocked(&mut st, key, Blocked::KeyChanged);
    }

    /// A key-mode handshake completed under `pin`. Latches the host if it was not already (the
    /// timer is NOT slid by later successes) and says so once, on that transition.
    pub(crate) fn key_established(key: &str, pin: &str, verify: Option<c_long>) {
        key_established_at(key, pin, verify, Instant::now());
    }

    /// [`key_established`] at `now`, the clock seam [`begin_at`] shares.
    pub(super) fn key_established_at(key: &str, pin: &str, verify: Option<c_long>, now: Instant) {
        key_established_in(key, pin, verify, now, wall_clock_year);
    }

    /// [`key_established_at`] with the year the device believes it is, the seam the engaged fact's
    /// year is tested through. Asked for only on the latch transition, not on every key-mode
    /// handshake.
    pub(super) fn key_established_in(
        key: &str,
        pin: &str,
        verify: Option<c_long>,
        now: Instant,
        year: impl FnOnce() -> Option<i64>,
    ) {
        let mut told_year = None;
        {
            let mut st = state();
            // A key-mode handshake completed: whatever was published as blocking this host is over.
            clear_blocked(&mut st, key);
            let live = st
                .latched
                .get(key)
                .is_some_and(|(p, since)| p == pin && now.saturating_duration_since(*since) < LATCH);
            if !live && st.table.get(key).map(String::as_str) == Some(pin) {
                st.latched.insert(key.to_owned(), (pin.to_owned(), now));
                let year = year();
                // The year of the FIRST engagement stands; a re-engagement after a lapse is not one.
                if !st.engaged.contains_key(key) {
                    let seq = st.engaged_seq;
                    st.engaged_seq += 1;
                    st.engaged.insert(key.to_owned(), (seq, year));
                    moved();
                }
                told_year = Some(year);
            }
        }
        if let Some(year) = told_year {
            crate::log(&engaged_line(verify, year));
        }
    }

    /// Key mode answered with a different key than the remembered one: stop serving the host in
    /// key mode, publish it ([`Blocked::KeyChanged`]) and say why.
    pub(super) fn key_refused(key: &str) {
        key_changed(key);
        crate::log("net: the server presented a different key than the remembered one — refusing");
    }

    /// The one line logged when a host first goes into key mode. Built on [`tls_verify_why`] so it
    /// carries the year the device believes it is, which is what lets a reader see a wrong clock.
    fn engaged_line(verify: Option<c_long>, year: Option<i64>) -> String {
        let why = match verify {
            Some(_) => tls_verify_why(TLS_VERIFY_FAILED, verify, year),
            None => None,
        }
        .unwrap_or_else(|| match year {
            Some(y) => format!("the device clock may be wrong (it believes the year is {y})"),
            None => "the device clock may be wrong".to_owned(),
        });
        format!("net: {why} — recognised the server by its remembered key")
    }

    /// Put `easy` in key mode, **failing closed**: the pin is set FIRST and its result checked, and
    /// only an accepted pin lets `CURLOPT_SSL_VERIFYPEER` go to 0. A libcurl or TLS backend that
    /// refuses the option leaves the handle exactly as strict as it was, and the caller fails the
    /// request as it would have. `CURLOPT_SSL_VERIFYHOST` is stated as 2 again, so the name check
    /// is on by this function's own text rather than by what a caller set earlier. `CERTINFO` is
    /// asked for so [`confirm`] can check the key itself too, in case a backend accepts the pin
    /// option and never enforces it.
    ///
    /// **Every key-mode handle does its own handshake.** `CURLOPT_FRESH_CONNECT` and
    /// `CURLOPT_FORBID_REUSE` are set before `VERIFYPEER` goes to 0, so the handle never takes a
    /// cached connection and leaves none behind. A reused connection performs no handshake: there
    /// would be no certificate for [`confirm`] to read (the open would be refused as a pin
    /// mismatch), and the pin and the name would not have been checked for this request at all. It
    /// also covers libcurl 7.53.1, where the pinned key is not part of the connection-reuse match,
    /// so a key-mode handle could otherwise ride a connection set up under different rules. The cost
    /// is one TLS handshake per key-mode request, accepted because key mode is a degraded state
    /// (a wrong clock) that the first strict success ends.
    ///
    /// # Safety
    /// `easy` is a live curl easy handle; `pin` outlives the transfer (libcurl 7.17+ copies it,
    /// but the caller holds it anyway).
    pub(crate) unsafe fn apply(easy: *mut CURL, pin: &CStr) -> Result<(), c_int> {
        let rc = unsafe { curl_easy_setopt_ptr(easy, CURLOPT_PINNEDPUBLICKEY, pin.as_ptr() as *const c_void) };
        if rc != 0 {
            return Err(rc);
        }
        // In this order: `VERIFYPEER` last, so it is only lowered once everything before it held.
        for (option, value) in [
            (CURLOPT_CERTINFO, 1),
            (CURLOPT_FRESH_CONNECT, 1),
            (CURLOPT_FORBID_REUSE, 1),
            (CURLOPT_SSL_VERIFYHOST, 2),
            (CURLOPT_SSL_VERIFYPEER, 0),
        ] {
            let rc = unsafe { curl_easy_setopt_long(easy, option, value) };
            if rc != 0 {
                return Err(rc);
            }
        }
        Ok(())
    }

    /// After a key-mode handshake completed: does the peer's leaf key really hash to `pin`? A
    /// second, independent check of what libcurl was asked to enforce.
    pub(crate) fn confirm(easy: *mut CURL, pin: &str) -> bool {
        peer_leaf_pin(easy).as_deref() == Some(pin)
    }

    /// Test seam: put `pin` in the table for `key` with no session behind it. Tests use a `key`
    /// that is unique to them (a loopback server's ephemeral port), so nothing leaks between the
    /// threads of the suite.
    #[cfg(test)]
    pub(crate) fn set_for_test(key: &str, pin: &str) {
        let mut st = state();
        if st.table.insert(key.to_owned(), pin.to_owned()).as_deref() != Some(pin) {
            st.latched.remove(key);
            clear_blocked(&mut st, key);
        }
    }

    /// Forget every published fact (not the tables, bindings or latches). For a test that asserts
    /// on a GLOBAL read ([`engaged`]) and so must not depend on what an earlier test engaged; the
    /// per-host tests need none, they key their own host and ask for it by machine.
    #[cfg(test)]
    pub(crate) fn reset_facts_for_test() {
        let mut st = state();
        st.blocked.clear();
        st.engaged.clear();
        st.engaged_seq = 0;
        moved();
    }

    #[cfg(test)]
    pub(crate) fn forget_for_test(key: &str) {
        let mut st = state();
        st.table.remove(key);
        st.latched.remove(key);
        clear_blocked(&mut st, key);
        st.engaged.remove(key);
        st.bindings.retain(|(_, hp)| hp != key);
    }

    #[cfg(test)]
    pub(crate) fn pin_for_test(key: &str) -> Option<String> {
        state().table.get(key).cloned()
    }

    /// Also read by the `tls-selftest` dev trigger, which reports which mode answered.
    #[cfg(any(test, feature = "devtriggers"))]
    pub(crate) fn is_latched(key: &str) -> bool {
        state().latched.contains_key(key)
    }

    /// Does the table hold a pin for `key` (`host:port`) right now? Read by the `tls-selftest` dev
    /// trigger, whose log lines report the table itself and not what the trigger last asked of it.
    #[cfg(any(test, feature = "devtriggers"))]
    pub(crate) fn holds(key: &str) -> bool {
        state().table.contains_key(key)
    }

    /// Removes a test's table and latch entries when it ends, however it ends.
    #[cfg(test)]
    pub(crate) struct Scoped(pub(crate) String);

    #[cfg(test)]
    impl Scoped {
        pub(crate) fn new(key: String, pin: &str) -> Self {
            set_for_test(&key, pin);
            Scoped(key)
        }

        /// Holds no key for `key`, but clears the facts a test published about it when it ends:
        /// loopback ports are reused, and a stale fact would answer a later test's host.
        pub(crate) fn watch(key: &str) -> Self {
            // Bound to a synthetic machine, as a registered server is: facts are published only
            // for bound hosts. [`forget_for_test`] unbinds it.
            Self::watch_machine(&format!("watch:{key}"), key)
        }

        /// [`watch`](Self::watch) with the machine named, for a test whose screen asks about that
        /// machine's server.
        pub(crate) fn watch_machine(machine: &str, key: &str) -> Self {
            if let Some((host, port)) = key.rsplit_once(':') {
                add_binding(&mut state(), machine, host, port.parse().unwrap_or(0));
            }
            Scoped(key.to_owned())
        }
    }

    #[cfg(test)]
    impl Drop for Scoped {
        fn drop(&mut self) {
            forget_for_test(&self.0);
        }
    }
}

#[cfg(test)]
pub(crate) fn with_test_response(reply: Vec<u8>, stall: bool, check: impl FnOnce(&str)) {
    request_tests::with_response(reply, stall, check);
}

/// Test-only input at the curl completion boundary, not a simulated wire exchange.
#[cfg(test)]
pub(crate) fn test_response_failure(rc: c_int, info_rc: c_int, code: c_long, redirects: bool)
    -> Result<Resp, RequestFailure> {
    let mut sink = BodySink::new(None);
    sink.push(b"synthetic partial body");
    finish_response(rc, info_rc, code, redirects, None, sink)
}

#[cfg(test)]
mod request_tests {
    use super::*;

    pub(super) fn with_response(reply: Vec<u8>, stall: bool, check: impl FnOnce(&str)) {
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let url = format!("http://127.0.0.1:{}/", server.local_addr().unwrap().port());
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let until = Instant::now() + Duration::from_secs(5);
                while !stop.load(Ordering::Acquire) && Instant::now() < until {
                    if let Ok((mut socket, _)) = crate::testnet::accept(&server) {
                        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                        let _ = socket.read(&mut [0; 4096]);
                        let _ = socket.write_all(&reply);
                        while stall && !stop.load(Ordering::Acquire) && Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            struct Stop<'a>(&'a AtomicBool);
            impl Drop for Stop<'_> { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
            let _stop = Stop(&stop);
            check(&url);
        });
    }

    fn truncated_refusal_keeps_status(status: u16) {
        let _serial = crate::testlock::serial();
        assert!(global_init() && available(), "this transport regression requires host libcurl");
        let reply = format!("HTTP/1.1 {status} Refused\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nshort");
        with_response(reply.into_bytes(), false, |url| {
            let failure = request_evidence(url, &[], "GET", None, API, false, None, None).err().unwrap();
            assert_eq!(failure.status, Some(status), "received refusal status was erased");
            assert_eq!(failure.cause, RequestError::Transport);
            assert_eq!(failure.body_limit, None);
        });
    }

    #[test]
    fn truncated_401_retains_response_evidence() { truncated_refusal_keeps_status(401); }

    #[test]
    fn truncated_403_retains_response_evidence() { truncated_refusal_keeps_status(403); }

    #[test]
    fn http2_reset_keeps_validated_final_status() {
        for rc in [16, 55, 92] {
            for code in [401, 403, 404, 410] {
                assert_eq!(response_status(rc, 0, code, false), Some(code as u16));
                assert_eq!(response_status(rc, 1, code, false), None);
                assert_eq!(response_status(rc, 0, code, true), None);
            }
            for code in [0, 99, 100, 199, 600, 65536] {
                assert_eq!(response_status(rc, 0, code, false), None);
            }
        }
    }

    #[test]
    fn ca_trusted_http2_wire_reset_retains_refusal() {
        use std::io::{BufRead, Write};
        use std::process::{Command, Stdio};
        let _serial = crate::testlock::serial();
        assert!(global_init() && available());
        struct Peer(std::process::Child);
        impl Drop for Peer {
            fn drop(&mut self) {
                if let Some(mut input) = self.0.stdin.take() { let _ = input.write_all(b"\n"); }
                let _ = self.0.wait();
            }
        }
        for status in [401, 403, 404, 410] {
            let mut peer = Peer(Command::new("python3")
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/src/net/h2_reset_fixture.py"))
                .arg(status.to_string()).stdin(Stdio::piped()).stdout(Stdio::piped())
                .stderr(Stdio::inherit()).spawn().expect("local Python/OpenSSL H2 fixture"));
            let mut output = std::io::BufReader::new(peer.0.stdout.take().unwrap());
            let mut ready = String::new(); output.read_line(&mut ready).unwrap();
            let ready: serde_json::Value = serde_json::from_str(&ready).expect("fixture startup");
            let url = format!("https://127.0.0.1:{}/", ready["port"].as_u64().unwrap());
            let response = request_tls_evidence(&url, &[], "GET", None,
                Timeouts { total_s: 5, ..API }, false, None,
                Tls::CaBundle(ready["ca"].as_str().unwrap()), None, false);
            let mut sent = String::new(); output.read_line(&mut sent).unwrap();
            assert_eq!(sent.trim(), "h2-reset-sent", "fixture must negotiate H2 and send RST_STREAM");
            let failure = response.err().expect("reset transfer cannot expose a partial body");
            assert_eq!(failure.status, Some(status));
            assert_eq!(failure.cause, RequestError::Transport);
            assert_eq!(failure.body_limit, None);
            crate::plex::account::test_refusal_evidence(status, Err(failure));
            peer.0.stdin.take().unwrap().write_all(b"\n").unwrap();
            assert!(peer.0.wait().unwrap().success());
        }
    }

    #[test]
    fn evidence_bounds_and_completion_use_the_real_request_path() {
        let _serial = crate::testlock::serial();
        assert!(global_init() && available());
        for status in [200, 401, 403] {
            for len in [31, 32, 33, 65536] {
                let mut reply = format!("HTTP/1.1 {status} Reply\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").into_bytes();
                reply.extend(vec![b'x'; len]);
                with_response(reply, false, |url| {
                    let response = request_evidence(url, &[], "GET", None, API, false, Some(32), None);
                    if len <= 32 {
                        let response = response.ok().expect("complete within-limit transfer");
                        assert_eq!(response.status, status);
                        assert_eq!(response.body.len(), len);
                    } else {
                        let failure = response.err().expect("no truncated response may escape");
                        assert_eq!(failure.status, Some(status));
                        assert_eq!(failure.body_limit, Some(32));
                        assert_eq!(failure.cause, RequestError::Transport);
                    }
                });
            }
        }
    }

    #[test]
    fn body_timeout_retains_status_but_legacy_projection_stays_timed_out() {
        let _serial = crate::testlock::serial();
        assert!(global_init() && available());
        let reply = b"HTTP/1.1 403 Refused\r\nContent-Length: 1000\r\n\r\nshort".to_vec();
        let t = Timeouts { total_ms: 200, ..API };
        with_response(reply.clone(), true, |url| {
            let failure = request_evidence(url, &[], "GET", None, t, false, None, None).err().unwrap();
            assert_eq!(failure.status, Some(403));
            assert_eq!(failure.cause, RequestError::TimedOut);
        });
        with_response(reply, true, |url| {
            assert!(matches!(request_result(url, &[], "GET", None, t, false, None, None), Err(RequestError::TimedOut)));
        });
    }

    #[test]
    fn absent_invalid_and_redirect_failure_status_are_not_evidence() {
        let _serial = crate::testlock::serial();
        assert!(global_init() && available());
        for reply in [Vec::new(), b"HTTP/1.1 999 Invalid\r\nContent-Length: 0\r\n\r\n".to_vec()] {
            with_response(reply, false, |url| {
                let failure = request_evidence(url, &[], "GET", None, API, false, None, None).err().unwrap();
                assert_eq!(failure.status, None);
            });
        }
        // Redirect target is the same synthetic server, whose one reply ends its accept loop.
        with_response(b"HTTP/1.1 302 Move\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n".to_vec(), false, |url| {
            let failure = request_evidence(url, &[], "GET", None, Timeouts { total_ms: 200, ..API }, true, None, None).err().unwrap();
            assert_eq!(failure.status, None, "a prior redirect is not final-origin evidence");
        });
    }

    #[test]
    fn evidence_validation_excludes_security_errors_and_invalid_getinfo() {
        for rc in [35, 60, 77, 90, 47, 6, 7, 8] {
            assert_eq!(response_status(rc, 0, 401, false), None, "rc={rc}");
        }
        for code in [-1, 0, 99, 600, 65536, c_long::MAX] {
            assert_eq!(response_status(0, 0, code, false), None);
        }
        assert_eq!(response_status(18, 1, 401, false), None);
        assert_eq!(response_status(18, 0, 100, false), None);
        for rc in [16, 18, 23, 28, 55, 56, 92] {
            assert_eq!(response_status(rc, 0, 401, false), Some(401));
            assert_eq!(response_status(rc, 0, 401, true), None);
        }
        let mut sink = BodySink::new(Some(4));
        assert!(!sink.push(b"synthetic-secret"));
        let failure = finish_response(23, 1, 401, false, Some(4), sink).err().unwrap();
        assert_eq!(failure.status, None);
        assert_eq!(failure.body_limit, Some(4));
        assert!(!format!("{failure:?}").contains("synthetic-secret"));
    }

    #[test]
    fn option_wrappers_preserve_complete_and_incomplete_projection() {
        let _serial = crate::testlock::serial();
        assert!(global_init() && available());
        for post in [false, true] {
            for complete in [false, true] {
                let length = if complete { 2 } else { 100 };
                with_response(format!("HTTP/1.1 401 Refused\r\nContent-Length: {length}\r\nConnection: close\r\n\r\nok").into_bytes(), false, |url| {
                    let response = if post { https_post(url, &[], b"") } else { https_get(url, &[]) };
                    assert_eq!(response.map(|r| r.status), complete.then_some(401));
                });
            }
        }
    }

    #[test]
    fn a_bounded_sink_refuses_before_it_allocates_past_the_limit() {
        let mut sink = BodySink::new(Some(4));
        assert!(sink.push(b"abc"));
        assert!(!sink.push(b"de"));
        assert_eq!(
            sink.body, b"abc",
            "the overflowing callback chunk is never appended"
        );
        assert!(sink.overflowed);
    }

    #[test]
    fn bulk_reads_keep_the_connect_deadline_and_drop_the_transfer_deadline() {
        assert_eq!(
            API,
            Timeouts {
                connect_s: 8,
                total_s: 25,
                total_ms: 0,
                low_speed_bps: 0,
                low_speed_s: 0,
            }
        );
        assert_eq!(
            BULK,
            Timeouts {
                connect_s: 8,
                total_s: 0,
                total_ms: 0,
                low_speed_bps: 1,
                low_speed_s: 30,
            }
        );
    }

    /// A loopback HTTP/1.1 server answering one request per connection with `ok`, counting the
    /// connections it accepted. The acceptor is stopped through the flag whether or not the body
    /// panicked (the scope joins before it reports).
    fn with_ok_server(
        bind: &str,
        body: impl FnOnce(u16, &std::sync::atomic::AtomicUsize),
    ) -> Option<()> {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicUsize;
        let srv = std::net::TcpListener::bind(bind).ok()?;
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).unwrap();
        let accepts = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    match crate::testnet::accept(&srv) {
                        Ok((mut s, _)) => {
                            accepts.fetch_add(1, Ordering::AcqRel);
                            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                            let mut buf = [0u8; 2048];
                            let _ = s.read(&mut buf);
                            let _ = s.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                            );
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            struct StopAll<'a>(&'a AtomicBool);
            impl Drop for StopAll<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _stop = StopAll(&stop);
            body(port, &accepts);
        });
        Some(())
    }

    /// **The offline fix, at the transport.** A host no resolver on earth answers for
    /// (`.invalid`, RFC 2606) reaches a loopback listener when the request carries a resolve
    /// entry naming it, and reaches nothing without one — so a request that succeeds here did so
    /// through `CURLOPT_RESOLVE` and not through DNS. Vacuous on a host with no libcurl.
    #[test]
    fn a_resolve_entry_dials_the_address_without_dns() {
        let _g = crate::testlock::serial();
        if !(global_init() && available()) {
            return;
        }
        with_ok_server("127.0.0.1:0", |port, accepts| {
            let url = format!("http://no-such-host.invalid:{port}/");
            let entry = format!("no-such-host.invalid:{port}:127.0.0.1");
            let t = Timeouts {
                connect_s: 3,
                total_s: 5,
                ..API
            };
            let r = request_result(&url, &[], "GET", None, t, false, None, Some(&entry))
                .expect("the pinned name reaches the listener");
            assert_eq!(r.status, 200);
            assert_eq!(r.body, b"ok");
            assert_eq!(accepts.load(Ordering::Acquire), 1);
            // and without the entry the same name is exactly as unreachable as it always was
            assert!(request_result(&url, &[], "GET", None, t, false, None, None).is_err());
            assert_eq!(accepts.load(Ordering::Acquire), 1, "nothing dialled");
        })
        .expect("loopback v4 binds");
    }

    /// The same over IPv6, in whichever entry syntax the bound libcurl takes — bare below 7.57.0,
    /// bracketed from it — chosen by `ResolvePin::entry` from the version this process captured.
    /// Skips where `::1` cannot be bound.
    #[test]
    fn a_v6_resolve_entry_dials_the_address_in_this_curls_syntax() {
        let _g = crate::testlock::serial();
        if !(global_init() && available()) {
            return;
        }
        let ran = with_ok_server("[::1]:0", |port, accepts| {
            let pin = crate::plex::ResolvePin::for_test(
                "no-such-host.invalid",
                port as i32,
                "::1".parse().unwrap(),
            );
            let entry = resolve::entry_of(&pin);
            assert_eq!(
                curl_version_num() >= crate::plex::origin::CURL_RESOLVE_BRACKETS_SINCE,
                entry.ends_with(":[::1]"),
                "entry {entry:?} for curl {:#x}",
                curl_version_num()
            );
            let url = format!("http://no-such-host.invalid:{port}/");
            let t = Timeouts {
                connect_s: 3,
                total_s: 5,
                ..API
            };
            let r = request_result(&url, &[], "GET", None, t, false, None, Some(&entry))
                .expect("the pinned name reaches the v6 listener");
            assert_eq!(r.status, 200);
            assert_eq!(accepts.load(Ordering::Acquire), 1);
        });
        if ran.is_none() {
            eprintln!("skipped: ::1 not bindable here");
        }
    }

    #[test]
    fn public_redirects_are_http_only_and_never_downgrade_tls() {
        assert_eq!(
            allowed_redirect_protocols(b"https://example.invalid/qr"),
            CURLPROTO_HTTPS
        );
        assert_eq!(
            allowed_redirect_protocols(b"HTTPS://example.invalid/qr"),
            CURLPROTO_HTTPS
        );
        assert_eq!(
            allowed_redirect_protocols(b"http://example.invalid/qr"),
            CURLPROTO_HTTP | CURLPROTO_HTTPS,
            "plaintext may upgrade, but the inverse is forbidden"
        );
        assert_eq!(PUBLIC_MAX_REDIRECTS, 5);
    }
}

/// The latch's clock, through the `*_at` seam: it lapses after [`keypin::LATCH`] and a later key-mode
/// success inside the interval does not slide it.
#[cfg(test)]
mod keypin_latch_tests {
    use super::keypin::{self, Mode};
    use std::time::{Duration, Instant};

    const SECOND: Duration = Duration::from_secs(1);

    fn pin() -> String {
        crate::spki::pin_from_spki_der(&[7; 8])
    }

    fn is_key(mode: &Mode) -> bool {
        matches!(mode, Mode::Key { .. })
    }

    #[test]
    fn the_latch_lapses_after_its_interval_and_a_new_success_starts_a_fresh_one() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("latch-lapse.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        let t0 = Instant::now();
        keypin::key_established_at(&key, &pin(), Some(10), t0);
        assert!(keypin::is_latched(&key));
        assert!(is_key(&keypin::begin_at(&key, t0 + keypin::LATCH - SECOND)), "live inside the interval");
        assert_eq!(keypin::begin_at(&key, t0 + keypin::LATCH), Mode::Strict, "over at the interval");
        assert!(!keypin::is_latched(&key), "a lapsed latch is dropped, not just ignored");

        let t1 = t0 + keypin::LATCH + SECOND;
        keypin::key_established_at(&key, &pin(), Some(10), t1);
        assert!(is_key(&keypin::begin_at(&key, t1 + SECOND)), "a new success latches again");
    }

    #[test]
    fn a_second_key_mode_success_inside_the_interval_does_not_extend_it() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("latch-no-slide.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        let t0 = Instant::now();
        keypin::key_established_at(&key, &pin(), Some(10), t0);
        keypin::key_established_at(&key, &pin(), Some(10), t0 + keypin::LATCH - SECOND);
        assert_eq!(
            keypin::begin_at(&key, t0 + keypin::LATCH + SECOND),
            Mode::Strict,
            "the interval runs from the FIRST success",
        );
    }

    // ---- The facts the app reads (`keypin::blocked_for`, `engaged`, `revision`). Every test keys its
    // own host, so the suite's threads do not share a fact.

    fn blocked_of(key: &str) -> Option<keypin::Blocked> {
        keypin::fact_for(key).blocked
    }

    #[test]
    fn a_date_failure_with_no_key_publishes_no_key_and_only_then() {
        let _serial = crate::testlock::serial();
        for (n, (rc, verify)) in [(60, Some(9)), (60, Some(10))].into_iter().enumerate() {
            let key = keypin::key_of("fact-nokey.invalid", n as i32);
            let _scoped = keypin::Scoped::watch(&key);
            keypin::strict_failure(&key, rc, verify);
            assert_eq!(blocked_of(&key), Some(keypin::Blocked::NoKey), "rc {rc} verify {verify:?}");
        }
        for (n, (rc, verify)) in
            [(51, Some(9)), (60, Some(20)), (60, Some(21)), (60, Some(18)), (60, Some(0)), (60, None), (28, Some(10))]
                .into_iter()
                .enumerate()
        {
            let key = keypin::key_of("fact-nokey-not.invalid", n as i32);
            let _scoped = keypin::Scoped::watch(&key);
            keypin::strict_failure(&key, rc, verify);
            assert_eq!(blocked_of(&key), None, "rc {rc} verify {verify:?} is not a date failure");
        }
        // A key held for the host means key mode can help, so nothing is blocked.
        let key = keypin::key_of("fact-nokey-held.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        keypin::strict_failure(&key, 60, Some(10));
        assert_eq!(blocked_of(&key), None);
    }

    #[test]
    fn a_date_failure_of_a_host_that_is_no_media_server_publishes_nothing() {
        let _serial = crate::testlock::serial();
        // plex.tv fails its date check on a wrong clock too, but it is not a server key mode could
        // ever serve and nothing but a plex.tv success would clear the fact: it would colour a Home
        // that failed for an unrelated reason.
        let before = keypin::revision();
        let tv = keypin::key_of("plex.tv", 443);
        keypin::strict_failure(&tv, 60, Some(10));
        assert_eq!(blocked_of(&tv), None);
        assert_eq!(keypin::revision(), before, "an unbound host moves nothing the app reads");

        // A server that has never learned a key IS covered, as the boot projection binds it from
        // the stored session before any request is made.
        let machine = "fact-bound-machine";
        let key = keypin::key_of("fact-bound.invalid", 32400);
        let _scoped = keypin::Scoped::watch(&key);
        keypin::project(Vec::new(), &[(machine.into(), "fact-bound.invalid".into(), 32400)], false);
        keypin::strict_failure(&key, 60, Some(10));
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::NoKey));
        keypin::project(Vec::new(), &[], true);
    }

    /// The dev `clockfact` plant is read like any fact and survives what clears a bound host's
    /// (a sign-out projection), since it belongs to no host a session could name.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn a_planted_fact_is_read_and_outlives_a_sign_out_projection() {
        let _serial = crate::testlock::serial();
        struct Forget;
        impl Drop for Forget {
            fn drop(&mut self) {
                keypin::forget_for_test(keypin::PLANTED_KEY);
            }
        }
        let _forget = Forget;
        // `engaged()` below is a global read: start from no engagement an earlier test left.
        keypin::reset_facts_for_test();
        keypin::plant(keypin::Planted::Blocked(keypin::Blocked::NoKey));
        assert_eq!(keypin::blocked_for("any-machine"), Some(keypin::Blocked::NoKey));
        keypin::project(Vec::new(), &[], true);
        assert_eq!(keypin::blocked_for("any-machine"), Some(keypin::Blocked::NoKey), "a projection touches bound hosts only");
        keypin::plant(keypin::Planted::Blocked(keypin::Blocked::KeyChanged));
        assert_eq!(keypin::blocked_for("any-machine"), Some(keypin::Blocked::KeyChanged));
        keypin::plant(keypin::Planted::Engaged(Some(2019)));
        assert_eq!(keypin::engaged(), Some(Some(2019)));
    }

    /// **Scenario A** (LAN-only cold boot, wrong clock, no key): NoKey stands, the viewer fixes the
    /// clock, the server is now unreachable for another reason. The fact is the host's LATEST strict
    /// outcome, so a strict failure that is not about the date ends it — and *Try again* no longer
    /// blames a clock that is right.
    #[test]
    fn a_later_strict_failure_that_is_not_the_date_clears_no_key_and_not_a_changed_key() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("fact-later.invalid", 1);
        let _scoped = keypin::Scoped::watch(&key);
        for (rc, verify) in [(7, None), (28, None), (60, Some(20)), (60, Some(18)), (51, Some(9)), (60, None)] {
            keypin::strict_failure(&key, 60, Some(10));
            assert_eq!(blocked_of(&key), Some(keypin::Blocked::NoKey));
            let before = keypin::revision();
            keypin::strict_failure(&key, rc, verify);
            assert_eq!(blocked_of(&key), None, "rc {rc} verify {verify:?} is not the date: the clock is not the cause");
            assert_ne!(keypin::revision(), before, "clearing a fact is a change a screen must see");
        }
        // Nothing to clear is not a change.
        let before = keypin::revision();
        keypin::strict_failure(&key, 7, None);
        assert_eq!(keypin::revision(), before);

        // A key that changed stays changed until a pin change or a success: an unrelated failure
        // says nothing about the key the host presented.
        keypin::key_changed(&key);
        keypin::strict_failure(&key, 7, None);
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::KeyChanged));
    }

    /// **Scenario B**: a second bound server's NoKey must not colour a read-out about another
    /// server. The fact is asked for BY MACHINE.
    #[test]
    fn a_fact_is_answered_for_the_machine_it_was_published_about_only() {
        let _serial = crate::testlock::serial();
        let (ka, kb) = (keypin::key_of("fact-scope-a.invalid", 1), keypin::key_of("fact-scope-b.invalid", 1));
        let kb2 = keypin::key_of("fact-scope-b2.invalid", 1);
        let _scoped = (
            keypin::Scoped::watch_machine("m-scope-a", &ka),
            keypin::Scoped::watch_machine("m-scope-b", &kb),
            keypin::Scoped::watch_machine("m-scope-b", &kb2),
        );
        keypin::strict_failure(&kb, 60, Some(10));
        assert_eq!(keypin::blocked_for("m-scope-b"), Some(keypin::Blocked::NoKey));
        assert_eq!(keypin::blocked_for("m-scope-a"), None, "server B's expired certificate is not server A's clock");
        assert_eq!(keypin::blocked_for("m-scope-unknown"), None);
        assert_eq!(keypin::blocked_for(""), None);

        // One machine, two addresses: the changed key outranks the missing one.
        keypin::key_changed(&kb2);
        assert_eq!(keypin::blocked_for("m-scope-b"), Some(keypin::Blocked::KeyChanged));
        assert_eq!(keypin::blocked_for("m-scope-a"), None);
    }

    #[test]
    fn a_changed_key_publishes_key_changed_and_outranks_no_key() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("fact-changed.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        keypin::key_refused(&key);
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::KeyChanged));

        // The media stack's quiet publisher says the same and keeps the latch rule.
        keypin::forget_for_test(&key);
        let _scoped2 = keypin::Scoped::new(key.clone(), &pin());
        let t0 = Instant::now();
        keypin::key_established_at(&key, &pin(), Some(10), t0);
        assert!(keypin::is_latched(&key));
        keypin::key_changed(&key);
        assert!(!keypin::is_latched(&key), "rc 90 still ends key mode for the host");
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::KeyChanged));
    }

    #[test]
    fn a_success_or_a_pin_change_clears_that_host_only() {
        let _serial = crate::testlock::serial();
        let a = keypin::key_of("fact-clear-a.invalid", 1);
        let b = keypin::key_of("fact-clear-b.invalid", 1);
        let c = keypin::key_of("fact-clear-c.invalid", 1);
        let (_a, _b, _c) = (keypin::Scoped::new(a.clone(), &pin()), keypin::Scoped::watch(&b), keypin::Scoped::new(c.clone(), &pin()));
        keypin::key_changed(&a);
        keypin::strict_failure(&b, 60, Some(10));
        keypin::key_changed(&c);
        assert_eq!((blocked_of(&a), blocked_of(&b), blocked_of(&c)), (
            Some(keypin::Blocked::KeyChanged),
            Some(keypin::Blocked::NoKey),
            Some(keypin::Blocked::KeyChanged),
        ));

        keypin::strict_established(&a);
        assert_eq!(blocked_of(&a), None, "a strict success clears its own host");
        assert_eq!(blocked_of(&b), Some(keypin::Blocked::NoKey), "…and not another's");

        keypin::key_established_at(&c, &pin(), Some(10), Instant::now());
        assert_eq!(blocked_of(&c), None, "a key-mode success clears its host");

        keypin::key_changed(&a);
        keypin::set_for_test(&a, &crate::spki::pin_from_spki_der(&[8; 8]));
        assert_eq!(blocked_of(&a), None, "a pin change clears it");
        assert_eq!(blocked_of(&b), Some(keypin::Blocked::NoKey));
    }

    #[test]
    fn a_pin_arriving_for_a_blocked_host_clears_no_key_and_a_projection_that_changes_nothing_does_not() {
        let _serial = crate::testlock::serial();
        let machine = "fact-project-machine";
        let key = keypin::key_of("fact-project.invalid", 1);
        let _scoped = keypin::Scoped::watch(&key);
        keypin::project(Vec::new(), &[(machine.into(), "fact-project.invalid".into(), 1)], false);
        keypin::strict_failure(&key, 60, Some(10));
        let before = keypin::revision();
        keypin::project(vec![("unrelated".into(), pin())], &[(machine.into(), "fact-project.invalid".into(), 1)], false);
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::NoKey), "still no key for this host");
        assert_eq!(keypin::revision(), before, "nothing changed, nothing published");

        keypin::project(vec![(machine.into(), pin())], &[(machine.into(), "fact-project.invalid".into(), 1)], false);
        assert_eq!(blocked_of(&key), None, "a key arriving makes key mode able to help");

        // Sign-out: an empty projection clears the facts of every bound host, including one that
        // never had a key (no pin removal says so for it).
        keypin::project(Vec::new(), &[], true);
        assert_eq!(keypin::pin_for_test(&key), None);
        keypin::strict_failure(&key, 60, Some(10));
        assert_eq!(blocked_of(&key), Some(keypin::Blocked::NoKey));
        keypin::project(Vec::new(), &[], true);
        assert_eq!(blocked_of(&key), None, "sign-out ends what was blocked");
    }

    #[test]
    fn the_engaged_year_is_the_first_one_and_survives_a_latch_lapse() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("fact-engaged.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        assert_eq!(keypin::fact_for(&key).engaged, None, "not engaged yet");
        let t0 = Instant::now();
        keypin::key_established_in(&key, &pin(), Some(10), t0, || Some(2020));
        assert_eq!(keypin::fact_for(&key).engaged, Some(Some(2020)));

        let t1 = t0 + keypin::LATCH + SECOND;
        assert_eq!(keypin::begin_at(&key, t1), Mode::Strict, "the latch lapsed");
        keypin::key_established_in(&key, &pin(), Some(10), t1, || Some(2031));
        assert_eq!(keypin::fact_for(&key).engaged, Some(Some(2020)), "the first engagement is the fact");
        assert!(keypin::engaged().is_some());
    }

    #[test]
    fn an_engagement_with_no_known_year_records_none_inside() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("fact-engaged-noyear.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        keypin::key_established_in(&key, &pin(), None, Instant::now(), || None);
        assert_eq!(keypin::fact_for(&key).engaged, Some(None));
    }

    #[test]
    fn the_revision_moves_on_each_change_and_not_otherwise() {
        let _serial = crate::testlock::serial();
        let key = keypin::key_of("fact-revision.invalid", 1);
        let _scoped = keypin::Scoped::new(key.clone(), &pin());
        let r0 = keypin::revision();
        keypin::strict_failure(&key, 60, Some(21));
        keypin::strict_failure(&key, 51, Some(10));
        keypin::strict_established(&key);
        assert_eq!(keypin::revision(), r0, "nothing was published or cleared");

        keypin::key_changed(&key);
        let r1 = keypin::revision();
        assert!(r1 > r0, "a fact appeared");
        keypin::key_changed(&key);
        assert_eq!(keypin::revision(), r1, "the same fact again is not a change");
        keypin::strict_established(&key);
        let r2 = keypin::revision();
        assert!(r2 > r1, "a fact cleared");

        keypin::key_established_in(&key, &pin(), Some(10), Instant::now(), || Some(2020));
        let r3 = keypin::revision();
        assert!(r3 > r2, "engagement is a fact");
        keypin::key_established_in(&key, &pin(), Some(10), Instant::now() + keypin::LATCH + SECOND, || Some(2021));
        assert_eq!(keypin::revision(), r3, "a re-engagement after a lapse adds nothing");
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::time::Duration;

    fn timeouts(total_s: c_long, total_ms: c_long) -> Timeouts {
        Timeouts { connect_s: 8, total_s, total_ms, low_speed_bps: 0, low_speed_s: 0 }
    }

    /// A key-mode retry spends what the failed strict attempt left of the caller's deadline, never a
    /// fresh whole one. (A request test would need the strict handshake to take a known time, which
    /// is a sleep; the arithmetic is what decides, so it is tested directly.)
    #[test]
    fn a_retry_gets_the_remaining_budget_and_none_when_nothing_remains() {
        let left = remaining_budget(timeouts(0, 5000), Duration::from_millis(1200)).unwrap();
        assert_eq!(left.total_ms, 3800);
        assert_eq!((left.connect_s, left.total_s), (8, 0), "nothing else about the request changes");
        let left = remaining_budget(timeouts(25, 0), Duration::from_millis(1500)).unwrap();
        assert_eq!(left.total_ms, 23_500, "a whole-second budget becomes a millisecond one");
        assert_eq!(remaining_budget(timeouts(0, 5000), Duration::from_millis(5000)), None);
        assert_eq!(remaining_budget(timeouts(0, 5000), Duration::from_millis(5001)), None);
        assert_eq!(remaining_budget(timeouts(25, 0), Duration::from_secs(60)), None);
    }

    #[test]
    fn a_request_with_no_whole_deadline_keeps_none() {
        let t = timeouts(0, 0);
        assert_eq!(remaining_budget(t, Duration::from_secs(3600)), Some(t));
    }
}

#[cfg(test)]
mod tls_verify_why_tests {
    use super::*;

    fn why(rc: c_int, verify: Option<c_long>) -> String {
        tls_verify_why(rc, verify, Some(1970)).expect("a TLS verification code")
    }

    /// A wrong television clock is the cause GitHub discussion #351 found behind rc=60, and the
    /// year the device believes it is is what lets a reporter's log confirm it.
    #[test]
    fn a_validity_window_failure_blames_the_clock_and_names_the_year() {
        assert_eq!(
            why(60, Some(9)),
            "peer certificate is not yet valid — the device clock may be wrong (it believes the year is 1970)"
        );
        assert_eq!(
            why(60, Some(10)),
            "peer certificate has expired — the device clock may be wrong (it believes the year is 1970)"
        );
        assert_eq!(
            tls_verify_why(60, Some(10), None).as_deref(),
            Some("peer certificate has expired — the device clock may be wrong")
        );
    }

    #[test]
    fn only_a_missing_issuer_is_blamed_on_the_ca_store() {
        for n in [20, 21] {
            assert!(why(60, Some(n)).contains("CA store too old?"), "verify result {n}");
        }
        for n in [9, 10, 18, 19, 7, 0] {
            assert!(!why(60, Some(n)).contains("CA store"), "verify result {n}");
        }
        assert!(!tls_verify_why(60, None, Some(2026)).unwrap().contains("CA store"));
    }

    #[test]
    fn self_signed_name_mismatch_and_unknown_results_each_say_so() {
        assert_eq!(why(60, Some(18)), "peer certificate is self-signed");
        assert_eq!(why(60, Some(19)), "peer certificate is self-signed");
        // 51 is decided by the code alone: OpenSSL keeps the name check out of the verify result.
        assert_eq!(why(51, Some(0)), "certificate name does not match host");
        assert_eq!(why(51, None), "certificate name does not match host");
        assert_eq!(why(60, Some(7)), "peer certificate could not be verified (X509 verify result 7)");
        // Zero after a failure and a getinfo that failed are both "the backend did not say"; on
        // libcurl 7.62+ that is how a name mismatch arrives (51 was retired), so the line says so.
        let unreported = "peer certificate could not be verified (no X509 verify result; on libcurl 7.62+ this is how a certificate name mismatch reads)";
        assert_eq!(why(60, Some(0)), unreported);
        assert_eq!(why(60, None), unreported);
    }

    #[test]
    fn other_codes_are_not_this_functions_to_explain() {
        for rc in [6, 7, 28, 35, 77, 90] {
            assert_eq!(tls_verify_why(rc, Some(10), Some(2026)), None, "rc {rc}");
        }
    }

    #[test]
    fn civil_date_is_the_gregorian_calendar() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(951_782_400), (2000, 2, 29));
        assert_eq!(civil_date(-1), (1969, 12, 31));
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;

    #[test]
    fn only_pre_1_1_openssl_requires_application_locks() {
        assert!(needs_legacy_crypto_locks(
            "libcurl/7.53.1 OpenSSL/1.0.2p zlib/1.2.11"
        ));
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
        assert!(threaded_tls_policy(
            "libcurl/8.7 SecureTransport",
            LegacyCrypto::NotNeeded
        ));
    }

    #[test]
    fn legacy_lock_helper_obeys_the_lock_bit() {
        let mut lock: libc::pthread_mutex_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::pthread_mutex_init(&mut lock, ptr::null()) },
            0
        );
        unsafe { apply_legacy_crypto_lock(&mut lock, 1) };
        assert_ne!(
            unsafe { libc::pthread_mutex_trylock(&mut lock) },
            0,
            "CRYPTO_LOCK must hold it"
        );
        unsafe { apply_legacy_crypto_lock(&mut lock, 2) };
        assert_eq!(
            unsafe { libc::pthread_mutex_trylock(&mut lock) },
            0,
            "unlock mode must release it"
        );
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
        assert!(
            std::path::Path::new(&got).is_absolute(),
            "curl is given an absolute path"
        );
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


#[cfg(test)]
mod peer_pin_tests {
    use super::*;
    use std::ffi::CString;

    /// A `curl_slist` of `lines`, laid out as libcurl lays it out. The nodes borrow the strings.
    struct List {
        _lines: Vec<CString>,
        nodes: Vec<CurlStringNode>,
    }

    impl List {
        fn of(lines: &[&str]) -> List {
            let lines: Vec<CString> = lines.iter().map(|l| CString::new(*l).unwrap()).collect();
            let mut nodes: Vec<CurlStringNode> = lines
                .iter()
                .map(|l| CurlStringNode { data: l.as_ptr(), next: ptr::null() })
                .collect();
            for i in 1..nodes.len() {
                let next = &nodes[i] as *const CurlStringNode;
                nodes[i - 1].next = next;
            }
            List { _lines: lines, nodes }
        }
        fn head(&self) -> *const CurlStringNode {
            self.nodes.first().map_or(ptr::null(), |n| n as *const CurlStringNode)
        }
    }

    fn minted() -> (String, String) {
        let cert = mint_cert(&["certinfo.invalid"]);
        (cert.pem.clone(), crate::spki::pin_from_spki_der(&cert.spki_der))
    }

    /// The pin of certificate 0 in a `curl_certinfo` made of `chain`.
    fn pin_of(chain: &[&List], num_of_certs: c_int) -> Option<String> {
        let heads: Vec<*const CurlStringNode> = chain.iter().map(|l| l.head()).collect();
        let info = CurlCertInfo { num_of_certs, certinfo: heads.as_ptr() };
        unsafe { leaf_pin_of_certinfo(&info) }
    }

    #[test]
    fn the_cert_line_of_certificate_zero_is_pinned() {
        let (leaf_pem, leaf_pin) = minted();
        let (issuer_pem, _) = minted();
        let leaf = List::of(&["Subject:CN=leaf", "Version:2", &format!("Cert:{leaf_pem}")]);
        let issuer = List::of(&[&format!("Cert:{issuer_pem}")]);
        assert_eq!(pin_of(&[&leaf, &issuer], 2), Some(leaf_pin));
    }

    #[test]
    fn nothing_readable_is_none() {
        let (pem, _) = minted();
        // No info at all, an empty chain, a null chain pointer.
        assert_eq!(unsafe { leaf_pin_of_certinfo(ptr::null()) }, None);
        let leaf = List::of(&[&format!("Cert:{pem}")]);
        assert_eq!(pin_of(&[&leaf], 0), None);
        assert_eq!(pin_of(&[&leaf], -1), None);
        let null_chain = CurlCertInfo { num_of_certs: 1, certinfo: ptr::null() };
        assert_eq!(unsafe { leaf_pin_of_certinfo(&null_chain) }, None);
        // An empty list for the leaf, a list with no `Cert:` line, and a `Cert:` that is not X.509.
        assert_eq!(pin_of(&[&List::of(&[])], 1), None);
        assert_eq!(pin_of(&[&List::of(&["Subject:CN=leaf", "Version:2"])], 1), None);
        assert_eq!(pin_of(&[&List::of(&["Cert:not a certificate"])], 1), None);
        assert_eq!(pin_of(&[&List::of(&["Cert:"])], 1), None);
        // Only certificate 0 is consulted: a good certificate behind a bad leaf is not the leaf.
        assert_eq!(pin_of(&[&List::of(&["Subject:CN=leaf"]), &leaf], 2), None);
    }

    #[test]
    fn the_walk_is_bounded_and_the_pem_is_capped() {
        // A list that points back at itself must end rather than spin.
        let mut looped = List::of(&["Subject:a"]);
        let head = looped.head();
        looped.nodes[0].next = head;
        assert_eq!(pin_of(&[&looped], 1), None);
        // A `Cert:` line past the node bound is never reached; one over the size cap is refused.
        let (pem, _) = minted();
        let mut lines = vec!["Subject:x"; CERTINFO_MAX_NODES];
        let late = format!("Cert:{pem}");
        lines.push(&late);
        assert_eq!(pin_of(&[&List::of(&lines)], 1), None);
        let huge = format!("Cert:{pem}{}", " ".repeat(CERTINFO_MAX_PEM));
        assert_eq!(pin_of(&[&List::of(&[&huge])], 1), None);
        // A node with a null string is skipped, not dereferenced.
        let (pem, pin) = minted();
        let mut list = List::of(&["x", &format!("Cert:{pem}")]);
        list.nodes[0].data = ptr::null();
        assert_eq!(pin_of(&[&list], 1), Some(pin));
    }

    /// Only an https request that verified against a trust store and cannot be redirected asks.
    #[test]
    fn only_a_strictly_verified_https_request_may_read_the_peer_key() {
        let ca = Tls::Ca;
        assert!(peer_pin_wanted("https://pms.plex.direct:32400/identity", &ca, false));
        assert!(peer_pin_wanted("HTTPS://pms.plex.direct:32400/identity", &Tls::CaBundle("/x.pem"), false));
        assert!(!peer_pin_wanted("http://192.168.0.2:32400/identity", &ca, false), "plaintext");
        assert!(!peer_pin_wanted("https://lab.local/x", &Tls::Pinned("sha256//x"), false), "verification off");
        assert!(!peer_pin_wanted("https://pms.plex.direct/identity", &ca, true), "a redirect can change the peer");
        assert!(!peer_pin_wanted("", &ca, false));
    }
}
