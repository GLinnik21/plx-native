//! Opening the opt-in bundled curl stack (`make CURLSTACK=1`, cargo feature `http3-curlstack`):
//! curl 8.23 and the nine libraries it links, built `-plx`-suffixed beside the binary by
//! `ci/build-curlstack.sh`. Compiled only with that feature and only on glibc (`RTLD_DEEPBIND`);
//! everywhere else `net::load_bundled` is a constant `None` and the system libcurl is the only one.
//!
//! This module only OPENS the libraries. `net::load_bundled` binds the `curl` table from the
//! handle it returns, and `curlio` binds its multi table from the same handle
//! (`net::bundled_curl`), so the choice between bundled and system curl is made once.
//!
//! TLS BACKEND IS GnuTLS, NOT OpenSSL, deliberately. The first build of this bundle used OpenSSL
//! 3.5.9, and on-device testing (webOS 5.6.2) found it fatal: the television's own
//! `/usr/lib/libcrypto.so.1.0.2` is already in the process (the system libcurl pulls it in, see
//! below), and a second OpenSSL major in the same process crashed deterministically, a known
//! class of bug (two majors fighting over process-global RNG, `atfork` and provider state), not
//! the symbol-interposition problem `RTLD_DEEPBIND` fixes: `RTLD_LOCAL` did not help, and the
//! crash got MORE deterministic with it. GnuTLS and its stack (Nettle/Hogweed, libtasn1, GMP)
//! share no code, symbol names or init-time state with OpenSSL.
//!
//! WHY `RTLD_DEEPBIND` on every one of the ten (`dynlib::Handle::open_in_deepbind`'s doc has the
//! mechanism): the television's `libplayerAPIs.so.1`, linked by every build of this app, has a
//! real `NEEDED` entry on the system `libcurl.so.4`, which is therefore in the GLOBAL scope before
//! `main` runs, together with its own `nghttp2` (its version string names `nghttp2/1.26.0`). Opened
//! the ordinary way, the bundled curl's internal calls between its own exported functions resolved
//! to the system copy through the PLT: a SIGSEGV inside `libcurl.so.4.5.0`, at a byte-for-byte
//! reproducible offset, on the first HTTPS request, every launch, even though every pointer this
//! app `dlsym`'d sat inside the bundled library. The same hazard applies to any of the nine
//! dependencies that makes an internal self-call, so they are opened the same way.

use plx_base::dynlib::{Handle, Scope};
use std::path::Path;

use super::BUNDLED_CURL_SONAME;

/// The nine libraries `libcurl-plx.so.4` links, in an order where each one's own `NEEDED` entries
/// are already mapped when it is opened. None carries an rpath (same as the bundled FFmpeg:
/// `ci/build-ffmpeg.sh`), so each is opened by absolute path first and the loader then finds it
/// under its SONAME when the next one names it, `ff.rs::load_libraries`'s pattern. Nothing in this
/// app calls into them by name; curl's own C code is the only caller, so they bind no table.
/// `ci/build-curlstack.sh` and the Makefile's `CURLSTACK_SONAMES` list the same ten files.
const DEPS: &[&str] = &[
    "libgmp-plx.so.10",
    "libtasn1-plx.so.6",
    "libnettle-plx.so.8",
    "libhogweed-plx.so.6",
    "libgnutls-plx.so.30",
    "libnghttp2-plx.so.14",
    "libnghttp3-plx.so.9",
    "libngtcp2-plx.so.16",
    "libngtcp2_crypto_gnutls-plx.so.8",
];

/// Open the bundle in `dir` (the app directory) and return the curl handle, or `None` when the
/// bundle is absent or any one library fails to open — the caller then falls back to the system
/// libcurl, the safe direction on a partial deploy.
pub(super) fn open(dir: &Path) -> Option<Handle> {
    if !dir.join(BUNDLED_CURL_SONAME).is_file() {
        plx_base::eventlog::log(&format!(
            "net: no {BUNDLED_CURL_SONAME} beside the binary — using the system libcurl"
        ));
        return None;
    }
    let mut gnutls = None;
    for &name in DEPS {
        match Handle::open_in_deepbind(Some(dir), &[name], Scope::Local) {
            Some((handle, _)) if name == "libgnutls-plx.so.30" => gnutls = Some(handle),
            Some(_) => {}
            None => {
                // Not fatal by itself: `libcurl-plx.so.4`'s own NEEDED entry then cannot be
                // satisfied, its `RTLD_NOW` open below fails, and the caller falls back.
                plx_base::eventlog::log(&format!(
                    "net: bundled curl dependency {name} did not load — falling back to the \
                     system libcurl"
                ));
            }
        }
    }
    // `gnutls_global_init()` here, synchronously, on the main thread, before any worker exists to
    // race on it. `curl_global_init(CURL_GLOBAL_ALL)` is documented to do this eagerly, but the
    // first on-device test crashed two worker threads at the IDENTICAL instruction address (two
    // warm fetches racing their first HTTPS request after a restored session), the signature of a
    // non-atomic lazy TLS-backend init. Calling it directly takes the question off the table.
    // GnuTLS reference-counts the call, so curl's own later one is harmless.
    if let Some(handle) = gnutls {
        match handle.sym("gnutls_global_init") {
            Some(init) if !init.is_null() => {
                // SAFETY: `gnutls_global_init` is `int gnutls_global_init(void)`.
                let init: unsafe extern "C" fn() -> std::os::raw::c_int =
                    unsafe { std::mem::transmute(init) };
                let rc = unsafe { init() };
                if rc != 0 {
                    plx_base::eventlog::log(&format!(
                        "net: gnutls_global_init returned {rc} — the bundled curl may be unsafe \
                         under concurrent use"
                    ));
                }
            }
            _ => plx_base::eventlog::log(
                "net: libgnutls-plx.so.30 loaded but exports no gnutls_global_init",
            ),
        }
    }
    // `RTLD_GLOBAL`, as on-device verification ran it: the curl table's own `dlsym`s go through
    // this handle either way, so this only affects what LATER loads could see.
    match Handle::open_in_deepbind(Some(dir), &[BUNDLED_CURL_SONAME], Scope::Global) {
        Some((handle, _)) => Some(handle),
        None => {
            plx_base::eventlog::log(&format!(
                "net: {BUNDLED_CURL_SONAME} did not open — falling back to the system libcurl"
            ));
            None
        }
    }
}
