//! Opening the opt-in bundled curl stack (`make CURLSTACK=1`, cargo feature `http3-curlstack`):
//! curl 8.23 and the six libraries it links, built `-plx`-suffixed beside the binary by
//! `ci/build-curlstack.sh`. Compiled only with that feature and only on glibc (`target_env =
//! "gnu"`, for `RTLD_DEEPBIND`); everywhere else `net::load_bundled` is a constant `None` and the
//! system libcurl is the only one.
//!
//! This module only OPENS the libraries. `net::load_bundled` binds the `curl` table from the
//! handle it returns, and `curlio` binds its multi table from the same handle
//! (`net::bundled_curl`), so the choice between bundled and system curl is made once.
//!
//! TLS BACKEND IS OpenSSL 3.5 (`libcrypto-plx.so.3`, `libssl-plx.so.3`), for its AArch32 ARMv8
//! Crypto Extension paths (AES, PMULL/GHASH, SHA-1, SHA-256), chosen at run time, so a CPU that has
//! them runs TLS on its own instructions instead of table code. The television's own OpenSSL (1.0.x
//! to 3.x by firmware) is usually in the process too, through the system libcurl below. The two
//! never meet: every library here is opened `RTLD_DEEPBIND` (the bundle binds its own calls to
//! itself) and `RTLD_LOCAL` (nothing of it joins the global scope the firmware's OpenSSL users
//! resolve against); the `-plx` SONAMEs and symbol-version file names come from OpenSSL's own
//! build; and `ci/build-curlstack.sh`'s `build_openssl` points its configuration and module
//! directories at a path no television has, so it reads none of the firmware's OpenSSL files.
//!
//! WHY `RTLD_DEEPBIND` on every one of the seven (`dynlib::Handle::open_in_deepbind`'s doc has the
//! mechanism): the television's `libplayerAPIs.so.1`, linked by every build with the Feed backend
//! (`PLAYBACK_BACKENDS=both|feed`, the shipping set; a URI-only build does not link it), has a
//! real `NEEDED` entry on the TV's libcurl (`libcurl.so.5` through 4.10.0, `libcurl.so.4` from
//! 5.3.1), which is therefore in the GLOBAL scope before `main` runs, together with its own `nghttp2` (its version string names `nghttp2/1.26.0`). Opened
//! the ordinary way, the bundled curl's internal calls between its own exported functions resolved
//! to the system copy through the PLT: a SIGSEGV inside `libcurl.so.4.5.0`, at a byte-for-byte
//! reproducible offset, on the first HTTPS request, every launch, even though every pointer this
//! app `dlsym`'d sat inside the bundled library. The same hazard applies to any of the six
//! dependencies that makes an internal self-call (libssl into libcrypto above all, whose exported
//! names the firmware's own `libcrypto.so.*` also exports), so they are opened the same way.

use plx_base::dynlib::Handle;
use std::path::Path;

use super::BUNDLED_CURL_SONAME;

/// The six libraries `libcurl-plx.so.4` links, in an order where each one's own `NEEDED` entries
/// are already mapped when it is opened. None carries an rpath (same as the bundled FFmpeg:
/// `ci/build-ffmpeg.sh`), so each is opened by absolute path first and the loader then finds it
/// under its SONAME when the next one names it, `ff.rs::load_libraries`'s pattern. Nothing in this
/// app calls into them by name; curl's own C code is the only caller, so they bind no table.
/// `ci/build-curlstack.sh` and the Makefile's `CURLSTACK_SONAMES` list the same seven files.
const DEPS: &[&str] = &[
    "libcrypto-plx.so.3",
    "libssl-plx.so.3",
    "libnghttp2-plx.so.14",
    "libnghttp3-plx.so.9",
    "libngtcp2-plx.so.16",
    "libngtcp2_crypto_ossl-plx.so.0",
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
    for &name in DEPS {
        if Handle::open_in_deepbind(Some(dir), &[name]).is_none() {
            // Not fatal by itself: `libcurl-plx.so.4`'s own NEEDED entry then cannot be
            // satisfied, its `RTLD_NOW` open below fails, and the caller falls back.
            plx_base::eventlog::log(&format!(
                "net: bundled curl dependency {name} did not load — falling back to the \
                 system libcurl"
            ));
        }
    }
    // No explicit TLS-library init here: `curl_global_init` (called by `net::global_init_once`
    // on the main thread, before any worker exists) runs `OPENSSL_init_ssl` itself, and OpenSSL
    // 1.1+ initialises lazily and thread-safely in any case.
    // `RTLD_LOCAL` like its six dependencies (`open_in_deepbind`'s doc): the curl and multi tables
    // `dlsym` through this handle, so nothing needs the stack in the global scope.
    match Handle::open_in_deepbind(Some(dir), &[BUNDLED_CURL_SONAME]) {
        Some((handle, _)) => Some(handle),
        None => {
            plx_base::eventlog::log(&format!(
                "net: {BUNDLED_CURL_SONAME} did not open — falling back to the system libcurl"
            ));
            None
        }
    }
}
