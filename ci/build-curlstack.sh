#!/bin/sh
# Cross-compile a vendored, HTTP/3-capable libcurl for the webOS ARM target: GMP, Nettle (+
# Hogweed), libtasn1, GnuTLS (TLS 1.3 + QUIC), nghttp2, ngtcp2 (+ its GnuTLS crypto glue),
# nghttp3, then curl itself, linked against all nine.
#
# OPT-IN: only `make CURLSTACK=1` runs this (the Makefile's CURLSTACK block), and only that build
# ships the result and compiles the loader for it (cargo feature `http3-curlstack`). A default
# build never reaches this script.
#
# WHY BUNDLE THIS ONE TOO. The television's own libcurl is pinned at 7.53.1 (`curlio.rs`'s module
# doc) — HTTP/1.1 and HTTP/2 only, no QUIC. A reverse proxy (e.g. Traefik) in front of a server may
# advertise `http3` on its entrypoint; the television never could speak it. Same reasoning as
# `ci/build-ffmpeg.sh`'s FFmpeg bundle: a system library this old cannot be grown a protocol, only
# replaced, and dlopen-by-absolute-path is what lets a replacement coexist with every webOS release
# this app still supports (`net::load_bundled` falls back to the television's own libcurl the
# moment this bundle is absent or any one of its NEEDED libraries fails to open; `net::curlstack`
# has the loader's side of the story).
#
# THE OUTPUT IS CACHED, keyed like `ci/build-ffmpeg.sh`'s build tree: under
# $PLX_BUILD_CACHE/curlstack/<key> (default ~/.cache/plxnative), where the key covers every input
# that changes a shipped byte (this script, the eight pins, the toolchain's fingerprint, arm-cc.py
# and the link-evidence checker it runs, inherited build variables, and the checkout path the
# build trees and `.pc` files name). A hit re-installs the eight `vendor/*-prefix` trees and
# builds nothing, which is what lets a manifest edit that does not touch the stack (an FFmpeg bump)
# re-run this script for free, and what CI's `curlstack` cross-build entry caches. A miss builds
# everything, then publishes it; set PLX_BUILD_CACHE= (empty) to skip the cache entirely.
#
# WHY GNUTLS, NOT OPENSSL. An earlier version of this script built the TLS backend on OpenSSL
# 3.5.9. It built, packaged and installed cleanly — and then crashed the app on every single
# launch, deterministically, inside the TELEVISION'S OWN system libcurl/libssl/libcrypto 1.0.2,
# the instant `net::global_init` ran (confirmed on-device: webOS 5.6.2, SIGSEGV addr=0x0 inside
# `libc`, called from the system `libcrypto.so.1.0.2`, same crash offset every time). Scoping our
# OpenSSL's `dlopen` to `RTLD_LOCAL` (so its exported symbols never reach the process's global
# scope) did NOT fix it — proving this was never a symbol-export collision, but a deeper
# OpenSSL-specific hazard: two different major OpenSSL versions mapped into one process corrupt
# each other's global init state (`atfork`/signal-handler/RNG bookkeeping), independent of
# `dlopen` scoping. GnuTLS and its dependencies (Nettle, GMP, libtasn1) live in a completely
# separate symbol and process-state namespace from OpenSSL — there is nothing for them to
# collide with, so this class of bug cannot recur. See `rust-modules/net/src/net/curlstack.rs`'s
# module doc for the device-side half of this story.
#
# THE SONAME-RENAME CONVENTION is the same `-plx` infix FFmpeg uses (`libavformat-plx.so.63`,
# ...): `libgmp-plx.so.10`, `libnettle-plx.so.8`, `libhogweed-plx.so.6`, `libtasn1-plx.so.6`,
# `libgnutls-plx.so.30`, `libnghttp2-plx.so.14`, `libnghttp3-plx.so.9`, `libngtcp2-plx.so.16`,
# `libngtcp2_crypto_gnutls-plx.so.8`, `libcurl-plx.so.4`. A renamed SONAME is not cosmetic:
# `net::curlstack` dlopens these by ABSOLUTE PATH beside the binary (no rpath survives
# FFmpeg's own configure-eval problem, and these build systems have the identical issue), so the
# bare name must never collide with anything the device itself might already carry.
#
# NO COMPILE-TIME NEON, EVER. The oldest supported device is a real ARM Cortex-A9
# (docs/measurements/softfloat-arm-tv-2026-09-07.md), and this project has a documented SIGILL
# incident from targeting newer ARM codegen (docs/simplification-audit-2026-07-18.md:
# `-C target-cpu=cortex-a53` SIGILLs on this SoC via a missing/different CP15 barrier). The NDK
# gcc's default target (cortex-a9/armv7-a/soft-float, Makefile:263) is exactly right for every one
# of these builds with ZERO `-march=`/`-mcpu=`/`-mfpu=` flags added — do not add them. GMP's own
# `--host=arm-webos-linux-gnueabi` match picks hand-written `mpn/arm/*.asm` — plain ARMv4-class
# integer routines (checked: no NEON mnemonic anywhere in them), not a CPU-specific variant.
# Nettle's "fat" ARM build (`Assembly files: arm/fat arm` in its own configure summary)
# runtime-dispatches NEON vs plain paths via `getauxval`, the same pattern OpenSSL's perlasm used
# — that stays on; it is not a compile-time ISA assumption.
#
# CC IS `ci/arm-cc.py`, THE SAME WRAPPER THE APP'S OWN LINK USES — not the raw NDK gcc. Every
# other vendored C/C++ library in this project already builds this way (`ci/build-sentry-native.sh`'s
# `CC="$ROOT/ci/arm-cc.py"`, `ci/build-ffmpeg.sh`'s `--cc=./plx-arm-cc.py`): the wrapper only
# intervenes on a LINK invocation (`-o` with no `-c`/`-S`/`-E`), passing every compile straight to
# the real compiler, and its archive-exclusion/link-evidence receipt is what
# `ci/check-packaged-elf.py` verifies against EVERY ELF in the shipped `.ipk` — not an app-only
# rule.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEPS="$ROOT/ci/deps.py"
WEBOS_SDK=${WEBOS_SDK:-"$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot"}
SYSROOT="$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot"
CC="$ROOT/ci/arm-cc.py"
AR="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-ar"
RANLIB="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-ranlib"
STRIP="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-strip"
PATCHELF=${PATCHELF:-patchelf}
CMAKE=${CMAKE:-cmake}
# None of these ten build with -g explicitly, yet every output below carries debug_info and
# embeds this host's absolute build path regardless (every project's `__FILE__` in an
# assert/debug macro) — the exact leak `ci/check-package.py`'s HOSTPATH scan exists to catch (its
# own doc: "v0.2.1 went out with the maintainer's working directory baked into all three bundled
# FFmpeg libraries"). This flag handles both the macro and the debug-info path for every compiler
# here; `scrub_buildpath` below handles everything it cannot reach (plain preprocessor DEFINE
# values, and paths baked into the NDK's own PREBUILT objects from its own GitHub Actions build).
FILEMAP="-ffile-prefix-map=$ROOT=/vendor/plx-curlstack-build32"

fail() { echo "build-curlstack: $*" >&2; exit 1; }
test -x "$CC" || fail "webOS NDK not found at $WEBOS_SDK (run make setup-env)"
command -v "$PATCHELF" >/dev/null 2>&1 || fail "patchelf is required (brew install patchelf)"
command -v "$CMAKE" >/dev/null 2>&1 || fail "cmake is required (brew install cmake)"

# `rename_soname`/`repoint_needed` run AFTER arm-cc.py's own link already produced its evidence
# receipt beside the original filename — renaming the file does not move it, so both move the
# evidence trio alongside, and both re-stamp `elf_sha256` afterward (`fix_elf_sha256`): patchelf
# rewrites the dynamic section (SONAME, NEEDED), which changes the file's bytes out from under
# the hash arm-cc.py recorded when it linked them.
#
# SHIPPED UNSTRIPPED, on purpose — same as FFmpeg's libraries (`$(FFMPEG_STAGED)`'s own staging
# rule). Stripping here too would mean this script, not just patchelf, changing bytes evidence was
# taken against, which `ci/stage-link-evidence.py`'s `--stripped` verification already exists to
# do safely, ONCE, at the one place that needs it: the `ipk` target's own staging loop. Let it.

fix_elf_sha256() {
    # arm-cc.py's receipt ($1.link.json) records the ELF's sha256 as IT linked it; patchelf
    # rewrites the dynamic section afterward (SONAME, NEEDED), so the file's bytes — and
    # therefore its hash — change. map_sha256/trace_sha256/compiler_sha256 stay valid (those
    # inputs never changed); only elf_sha256 needs to catch up to the file as it actually ships.
    python3 -c "
import hashlib, json, sys
p = sys.argv[1] + '.link.json'
r = json.load(open(p))
r['elf_sha256'] = hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest()
json.dump(r, open(p, 'w'), indent=2)
open(p, 'a').write('\n')
" "$1"
}

scrub_buildpath() {
    # $1 = file. `-ffile-prefix-map` (set via $FILEMAP in every build below) only rewrites
    # `__FILE__`/debug-info paths — it does nothing for a path baked in as a plain preprocessor
    # DEFINE VALUE, nor for a FAMILY of paths baked into PREBUILT objects the NDK itself ships
    # (`libgcc.a`/crt*.o, implicitly linked into everything): `/Users/runner/work/native-toolchain/
    # ...`, that toolchain's OWN GitHub Actions build path, varying by WHICH libgcc source file
    # each debug-info entry names (`.../src/gcc/libgcc`, `.../src/gcc/libgcc/config/arm`, ...) —
    # not a single fixed string, and not something `--ffile-prefix-map` could reach even in
    # principle: it predates this script by however long ago the NDK vendor built it.
    #
    # So: the SAME pattern `ci/check-package.py`'s own HOSTPATH scan looks for
    # (`/(?:Users|home)/[A-Za-z0-9_./+-]+`, an anchored run of path-safe characters), applied here
    # as a find-and-replace instead of a scan — every match replaced with an EQUAL-LENGTH,
    # non-`/Users`, non-`/home` string, since any other length shifts every offset after it in an
    # ELF. $ROOT itself (my own real build path) is one more thing this same pattern already
    # matches, so it needs no separate case.
    [ -f "$1" ] || return 0
    python3 -c "
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
data = path.read_bytes()
pattern = re.compile(rb'/(?:Users|home)/[A-Za-z0-9_./+-]+')
def replace(m):
    old = m.group(0)
    return ('/vendor-build-' + '0' * len(old)).encode()[:len(old)]
new_data = pattern.sub(replace, data)
if new_data != data:
    assert len(new_data) == len(data)
    path.write_bytes(new_data)
" "$1"
    [ -f "$1.link.json" ] && fix_elf_sha256 "$1"
}

find_evidence_by_content() {
    # $1 = the installed file, $2 = the build tree to search. `make`/`cmake --install` COPIES the
    # linked .so from the build tree into the prefix — a separate file, byte-identical, but a
    # different path — so arm-cc.py's evidence (written beside wherever the ORIGINAL link
    # command's `-o` pointed, inside $2) never travels with it. `ci/stage-link-evidence.py` has a
    # `find_evidence_by_content` for exactly this shape, but it is Cargo-specific (walks up
    # looking for a `release`/`tvdev`/`debug` profile directory, which none of these build trees
    # have), so this is the same idea, generic: match $1's sha256 against every `*.link.json`
    # under $2, and copy (never move — more than one renamed output can share one evidence trio,
    # e.g. ngtcp2's two libraries from one build) the trio beside $1.
    # CMake's own install step can rewrite a library's RPATH in place ("Set non-toolchain portion
    # of runtime path... to ''"), changing its bytes AFTER arm-cc.py recorded its hash — seen on
    # ngtcp2's crypto glue library specifically (it is the one of these outputs with a build-time
    # RPATH to strip). Content hash is tried first; falling back to an exact basename match and
    # letting `rename_soname`'s `fix_elf_sha256` catch the hash up afterward covers that case too.
    python3 -c "
import hashlib, json, pathlib, shutil, sys
target, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
digest = hashlib.sha256(target.read_bytes()).hexdigest()
candidates = list(root.rglob('*.link.json'))
by_name = None
for j in candidates:
    try:
        record = json.loads(j.read_text())
    except (OSError, ValueError):
        continue
    if j.name == target.name + '.link.json':
        by_name = j
    if record.get('elf_sha256') == digest:
        base = str(j)[:-len('.link.json')]
        for suffix in ('.link.map', '.link.trace', '.link.json'):
            shutil.copy(base + suffix, str(target) + suffix)
        sys.exit(0)
if by_name is not None:
    base = str(by_name)[:-len('.link.json')]
    for suffix in ('.link.map', '.link.trace', '.link.json'):
        shutil.copy(base + suffix, str(target) + suffix)
    sys.exit(0)
sys.exit(f'no link evidence under {root} matches {target} by content or name (sha256 {digest})')
" "$1" "$2"
}

rename_soname() {
    # $1 = file as the build left it (with evidence already materialized beside it by
    # find_evidence_by_content), $2 = the final -plx name it should carry (same basename).
    for suffix in "" .link.map .link.trace .link.json; do
        [ -f "$1$suffix" ] && mv "$1$suffix" "$2$suffix"
    done
    "$PATCHELF" --set-soname "$2" "$2"
    [ -f "$2.link.json" ] && fix_elf_sha256 "$2"
}

repoint_needed() {
    # $1 = file, $2 = old NEEDED name, $3 = new NEEDED name.
    "$PATCHELF" --replace-needed "$2" "$3" "$1"
    [ -f "$1.link.json" ] && fix_elf_sha256 "$1"
}

build_gmp() {
    # SHARED, deliberately, even though GMP is normally static-only in most builds: the webOS
    # NDK's OWN sysroot already ships a real `libgmp.so.10` (GCC's own cross-toolchain build
    # depends on GMP, and this buildroot-derived NDK bundles the target-side copy it built with) —
    # confirmed by letting nettle's build run with `--disable-shared` on GMP first and finding
    # `libgmp.so.10` as an EXTRA, unrenamed NEEDED entry in the resulting libhogweed.so: the
    # cross-linker found the sysroot's ambient copy ahead of, or regardless of, our own `-L`
    # order, because `--sysroot` injects its own default library search paths at a fixed point
    # GCC's spec files control, not whatever position `-L` appears at on our command line. A
    # STATIC gmp.a cannot create that conflict (nothing to find instead of linking it in
    # directly) — so go shared and own the SONAME ourselves, same as everything else here, rather
    # than fight the sysroot's search order with linker tricks of increasing fragility.
    local archive src build prefix
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get gmp archive)"
    python3 "$DEPS" fetch gmp >/dev/null
    src="$ROOT/vendor/gmp-src"; build="$ROOT/vendor/gmp-build"; prefix="$ROOT/vendor/gmp-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src" "$build"
    tar xJf "$archive" --strip-components=1 -C "$src"
    (
        cd "$build"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            --enable-shared --disable-static --with-pic \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" \
            CFLAGS="--sysroot=$SYSROOT $FILEMAP" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT"
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libgmp.so.10.* 2>/dev/null | head -1); [ -z "$real" ] && real=libgmp.so.10
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libgmp-plx.so.10
      scrub_buildpath libgmp-plx.so.10
      # Restore a bare "libgmp.so" link-time name pointing at OUR renamed file: nettle's, gnutls's
      # and curl's own `./configure` still probe/link against a bare "-lgmp" for certain checks
      # (hogweed.pc's Libs.private, gnutls's final link), and this is the same symlink-shim
      # resolution trick `build_curl`'s own shim directory already relies on — the NEEDED entry
      # `ld` records is the TARGET's internal DT_SONAME (already "libgmp-plx.so.10"), not this
      # symlink's name, so it stays correct regardless of which bare name found it.
      ln -sf libgmp-plx.so.10 libgmp.so
    )
}

build_nettle() {
    # Needs GMP; produces TWO shared libraries (libnettle, and libhogweed for the public-key
    # primitives GnuTLS needs). nettle's own `AC_CHECK_LIB(gmp, __gmpn_zero_p, ...)` unconditionally
    # APPENDS a bare "-lgmp" to LIBS on success — it does not replace an existing LIBS override —
    # so simply passing `LIBS="-l:libgmp-plx.so.10"` produces `LIBS = -lgmp -l:libgmp-plx.so.10`
    # in the generated config.make, and `-lgmp` would resolve via the symlink shim `build_gmp`
    # leaves behind, giving a harmless but untidy EXTRA NEEDED entry pointing at the SAME file
    # under its renamed SONAME twice. Pre-seeding the check's cache variable to suppress the
    # append entirely (`ac_cv_lib_gmp___gmpn_zero_p=no`) also disables hogweed/public-key support
    # (that cached answer gates the feature, not just the LIBS append) — so instead: let the
    # check run normally, then strip the bare "-lgmp " out of the GENERATED config.make before
    # `make` ever reads it. A build-flag edit after generation, not a patch to any vendored
    # source.
    local archive src build prefix gmp_prefix
    gmp_prefix="$ROOT/vendor/gmp-prefix"
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get nettle archive)"
    python3 "$DEPS" fetch nettle >/dev/null
    src="$ROOT/vendor/nettle-src"; build="$ROOT/vendor/nettle-build"; prefix="$ROOT/vendor/nettle-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src" "$build"
    tar xzf "$archive" --strip-components=1 -C "$src"
    (
        cd "$build"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            --disable-static --enable-shared --disable-documentation \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 $FILEMAP" \
            CPPFLAGS="--sysroot=$SYSROOT -I$gmp_prefix/include" \
            LDFLAGS="--sysroot=$SYSROOT -L$gmp_prefix/lib" \
            LIBS="-l:libgmp-plx.so.10"
        sed -i.bak 's/^LIBS = -lgmp /LIBS = /' config.make && rm -f config.make.bak
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libnettle.so.8.* 2>/dev/null | head -1); [ -z "$real" ] && real=libnettle.so.8
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libnettle-plx.so.8
      scrub_buildpath libnettle-plx.so.8
      real=$(ls libhogweed.so.6.* 2>/dev/null | head -1); [ -z "$real" ] && real=libhogweed.so.6
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libhogweed-plx.so.6
      repoint_needed libhogweed-plx.so.6 libnettle.so.8 libnettle-plx.so.8
      scrub_buildpath libhogweed-plx.so.6
      ln -sf libnettle-plx.so.8 libnettle.so
      ln -sf libhogweed-plx.so.6 libhogweed.so
    )
}

build_libtasn1() {
    local archive src build prefix
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get libtasn1 archive)"
    python3 "$DEPS" fetch libtasn1 >/dev/null
    src="$ROOT/vendor/libtasn1-src"; build="$ROOT/vendor/libtasn1-build"; prefix="$ROOT/vendor/libtasn1-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src" "$build"
    tar xzf "$archive" --strip-components=1 -C "$src"
    (
        cd "$build"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            --disable-static --enable-shared --disable-doc \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 $FILEMAP" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT"
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libtasn1.so.6.* 2>/dev/null | head -1); [ -z "$real" ] && real=libtasn1.so.6
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libtasn1-plx.so.6
      scrub_buildpath libtasn1-plx.so.6
      ln -sf libtasn1-plx.so.6 libtasn1.so
    )
}

build_gnutls() {
    # Minimised to what an HTTPS client actually needs: no PKCS#11, no TPM, no DANE/unbound, no
    # C++ library, no NLS, no tools/tests/docs. `--with-included-unistring` sidesteps needing a
    # sixth vendored library — GnuTLS can bundle its own copy rather than requiring a system one.
    # `--without-zstd --without-brotli` matters beyond tidiness: left on, GnuTLS's configure
    # auto-detects the MAC'S OWN host zstd via pkg-config/brew and links against it, which is a
    # host-architecture library a cross-compiled .so cannot actually use — curl's later build
    # surfaced this as "undefined reference to ZSTD_compressBound" the first time through, not as
    # an immediate failure here (GnuTLS's own link only needed symbols zstd satisfies lazily).
    #
    # NETTLE_LIBS/HOGWEED_LIBS/LIBTASN1_LIBS override pkg-config entirely (cross-compiling pkg-config
    # itself is more fragile than just saying what we already know) and use `-l:exact-name.so.N`
    # syntax against our ALREADY-RENAMED dependencies, so GnuTLS's own build never touches an
    # unrenamed bare name at all — no post-link `repoint_needed` dance needed for this one.
    #
    # `--with-default-trust-store-dir`: without it, GnuTLS has NO idea where root CAs live on
    # this device when curl does not pass an explicit `CURLOPT_CAINFO` (net.rs's own CA logic
    # only sets that on specific fallback paths — see its own doc — so the FIRST, ordinary
    # connection attempt relies entirely on whatever default the TLS backend was built with). The
    # television's system OpenSSL gets this for free (its own default CAPATH, `/etc/ssl/certs`,
    # is OpenSSL's own hashed-symlink convention); GnuTLS has no equivalent auto-discovery once
    # `--without-p11-kit` removes its usual system-trust mechanism. Confirmed on-device: every
    # connection through the bundled stack failed with `GNUTLS_CERT_SIGNER_NOT_FOUND` (bit 1<<6 —
    # decode `CURLINFO_SSL_VERIFYRESULT`'s raw bitmask against GnuTLS's OWN enum, not OpenSSL's
    # X509_V_ERR_* numbering net.rs's other comments describe; the two backends use completely
    # different schemes for the same option).
    #
    # NOT `--with-default-trust-store-file=/etc/ssl/certs/ca-certificates.crt`: tried first, still
    # failed the exact same way. The concatenated bundle on this firmware is STALE relative to its
    # own `/etc/ssl/certs/` hash-symlink directory — `grep -c ISRG ca-certificates.crt` is 0, even
    # though `ISRG_Root_X1.pem` exists as its own file right beside it (confirmed inside the app's
    # own jail via `/proc/<pid>/root/etc/ssl/certs/`, which does bind-mount `/etc/ssl` — jail
    # visibility was never the problem). The individual per-cert files are current; only the
    # pre-concatenated file was never regenerated after a later cert update. Pointing GnuTLS at the
    # DIRECTORY instead makes it read every individual cert file itself — the same files OpenSSL's
    # own hashed-CAPATH lookup already reads — so it sees ISRG Root X1 regardless of the stale
    # bundle.
    local archive src build prefix nettle_prefix tasn1_prefix gmp_prefix
    nettle_prefix="$ROOT/vendor/nettle-prefix"; tasn1_prefix="$ROOT/vendor/libtasn1-prefix"; gmp_prefix="$ROOT/vendor/gmp-prefix"
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get gnutls archive)"
    python3 "$DEPS" fetch gnutls >/dev/null
    src="$ROOT/vendor/gnutls-src"; build="$ROOT/vendor/gnutls-build"; prefix="$ROOT/vendor/gnutls-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src" "$build"
    tar xJf "$archive" --strip-components=1 -C "$src"
    (
        cd "$build"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            --disable-static --enable-shared \
            --without-p11-kit --disable-doc --disable-tests --disable-tools --disable-nls \
            --without-idn --disable-cxx --disable-libdane --without-tpm2 --without-tpm \
            --disable-rpath --with-included-unistring --without-zstd --without-brotli \
            --with-default-trust-store-dir=/etc/ssl/certs \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 $FILEMAP" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT" \
            NETTLE_CFLAGS="-I$nettle_prefix/include" NETTLE_LIBS="-L$nettle_prefix/lib -l:libnettle-plx.so.8" \
            HOGWEED_CFLAGS="-I$nettle_prefix/include" HOGWEED_LIBS="-L$nettle_prefix/lib -l:libhogweed-plx.so.6 -l:libnettle-plx.so.8" \
            LIBTASN1_CFLAGS="-I$tasn1_prefix/include" LIBTASN1_LIBS="-L$tasn1_prefix/lib -l:libtasn1-plx.so.6"
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libgnutls.so.30.* 2>/dev/null | head -1); [ -z "$real" ] && real=libgnutls.so.30
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libgnutls-plx.so.30
      # GnuTLS's own link records a NEEDED of bare `libgmp.so.10`, not our renamed
      # `libgmp-plx.so.10`, even though hogweed's own link (same GMP, same -L path) gets the
      # rename right — almost certainly stale libtool `.la` dependency_libs bookkeeping captured
      # before gmp was renamed. Observed on-device: dlopen of the bundled curl stack fails
      # closed on this one NEEDED entry alone, silently falling back to the television's own
      # libcurl with no error beyond "dependency did not load" (plx_base::dynlib only reports
      # which candidate failed, not why — there is no gnutls-side symptom to grep for here other
      # than this exact NEEDED mismatch). Same fix as `libssl-plx.so.3`'s `libcrypto` reference.
      repoint_needed libgnutls-plx.so.30 libgmp.so.10 libgmp-plx.so.10
      scrub_buildpath libgnutls-plx.so.30
      ln -sf libgnutls-plx.so.30 libgnutls.so
    )
}

build_nghttp2() {
    local archive src build prefix
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get nghttp2 archive)"
    python3 "$DEPS" fetch nghttp2 >/dev/null
    src="$ROOT/vendor/nghttp2-src"; build="$ROOT/vendor/nghttp2-build"; prefix="$ROOT/vendor/nghttp2-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src" "$build"
    tar xzf "$archive" --strip-components=1 -C "$src"
    (
        cd "$build"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 -ffunction-sections -fdata-sections $FILEMAP" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT -Wl,--gc-sections" \
            --enable-lib-only --disable-static --enable-shared
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libnghttp2.so.14.* 2>/dev/null | head -1); [ -z "$real" ] && real=libnghttp2.so.14
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libnghttp2-plx.so.14
      scrub_buildpath libnghttp2-plx.so.14
      ln -sf libnghttp2-plx.so.14 libnghttp2.so
    )
}

build_nghttp3() {
    local archive src build prefix
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get nghttp3 archive)"
    python3 "$DEPS" fetch nghttp3 >/dev/null
    src="$ROOT/vendor/nghttp3-src"; build="$ROOT/vendor/nghttp3-build"; prefix="$ROOT/vendor/nghttp3-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src"
    tar xzf "$archive" --strip-components=1 -C "$src"
    "$CMAKE" -S "$src" -B "$build" \
        -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=arm \
        -DCMAKE_C_COMPILER="$CC" -DCMAKE_AR="$AR" -DCMAKE_RANLIB="$RANLIB" \
        -DCMAKE_SYSROOT="$SYSROOT" -DCMAKE_FIND_ROOT_PATH="$SYSROOT" \
        -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY \
        -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY \
        -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
        -DENABLE_LIB_ONLY=ON -DENABLE_STATIC_LIB=OFF -DENABLE_SHARED_LIB=ON \
        -DCMAKE_C_FLAGS="-ffunction-sections -fdata-sections $FILEMAP" \
        -DCMAKE_SHARED_LINKER_FLAGS="-Wl,--gc-sections"
    "$CMAKE" --build "$build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
    "$CMAKE" --install "$build"
    ( cd "$prefix/lib"
      real=$(ls libnghttp3.so.9.* 2>/dev/null | head -1); [ -z "$real" ] && real=libnghttp3.so.9
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libnghttp3-plx.so.9
      scrub_buildpath libnghttp3-plx.so.9
      ln -sf libnghttp3-plx.so.9 libnghttp3.so
    )
}

build_ngtcp2() {
    # `-DENABLE_GNUTLS=ON -DENABLE_OPENSSL=OFF`: ngtcp2's CMake `find_package(GnuTLS 3.7.2
    # REQUIRED)` wants the CACHE variables `GNUTLS_INCLUDE_DIR`/`GNUTLS_LIBRARY` — set them
    # directly to our renamed build rather than fighting cross-compiled pkg-config detection.
    # `CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY` is CRITICAL here, not optional: without it
    # the symbol probe this find_package performs tries to link a full EXECUTABLE against our
    # cross sysroot, which fails in a way indistinguishable from "GnuTLS genuinely lacks the
    # symbol" — it does not (confirmed: the real built header has it) — this is purely a
    # cross-compile try_compile artifact, the same class of gotcha `ci/build-sentry-native.sh`'s
    # own doc describes for its CXX-detection problem.
    # ngtcp2's CMakeLists also `enable_language()`s CXX unconditionally (examples/tests exist even
    # with BUILD_TESTING=OFF); left unset, CMake derives a companion CXX from the HOST's own
    # default (`/usr/bin/c++`) rather than the cross toolchain. Point it at the real cross g++
    # directly: nothing here links a C++ object.
    local archive src build prefix gnutls_prefix
    gnutls_prefix="$ROOT/vendor/gnutls-prefix"
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get ngtcp2 archive)"
    python3 "$DEPS" fetch ngtcp2 >/dev/null
    src="$ROOT/vendor/ngtcp2-src"; build="$ROOT/vendor/ngtcp2-build"; prefix="$ROOT/vendor/ngtcp2-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src"
    tar xzf "$archive" --strip-components=1 -C "$src"
    "$CMAKE" -S "$src" -B "$build" \
        -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=arm \
        -DCMAKE_C_COMPILER="$CC" -DCMAKE_CXX_COMPILER="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-c++" \
        -DCMAKE_AR="$AR" -DCMAKE_RANLIB="$RANLIB" \
        -DCMAKE_SYSROOT="$SYSROOT" -DCMAKE_FIND_ROOT_PATH="$SYSROOT;$gnutls_prefix" \
        -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY \
        -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY \
        -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
        -DGNUTLS_INCLUDE_DIR="$gnutls_prefix/include" -DGNUTLS_LIBRARY="$gnutls_prefix/lib/libgnutls-plx.so.30" \
        -DENABLE_GNUTLS=ON -DENABLE_OPENSSL=OFF \
        -DENABLE_STATIC_LIB=OFF -DENABLE_SHARED_LIB=ON -DBUILD_TESTING=OFF \
        -DCMAKE_C_FLAGS="-ffunction-sections -fdata-sections $FILEMAP" \
        -DCMAKE_SHARED_LINKER_FLAGS="-Wl,--gc-sections"
    "$CMAKE" --build "$build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
    "$CMAKE" --install "$build"
    ( cd "$prefix/lib"
      real1=$(ls libngtcp2.so.16.* 2>/dev/null | head -1); [ -z "$real1" ] && real1=libngtcp2.so.16
      real2=$(ls libngtcp2_crypto_gnutls.so.8.* 2>/dev/null | head -1); [ -z "$real2" ] && real2=libngtcp2_crypto_gnutls.so.8
      find_evidence_by_content "$real1" "$build"
      find_evidence_by_content "$real2" "$build"
      rename_soname "$real1" libngtcp2-plx.so.16
      rename_soname "$real2" libngtcp2_crypto_gnutls-plx.so.8
      repoint_needed libngtcp2_crypto_gnutls-plx.so.8 libngtcp2.so.16 libngtcp2-plx.so.16
      scrub_buildpath libngtcp2-plx.so.16
      scrub_buildpath libngtcp2_crypto_gnutls-plx.so.8
      ln -sf libngtcp2-plx.so.16 libngtcp2.so
      ln -sf libngtcp2_crypto_gnutls-plx.so.8 libngtcp2_crypto_gnutls.so
    )
}

build_curl() {
    # `PKG_CONFIG_PATH` spans EVERY prefix, not just ngtcp2's/gnutls's own: curl's configure
    # resolves `libngtcp2_crypto_gnutls`'s pkg-config `Requires.private: libngtcp2, gnutls`
    # transitively, and gnutls.pc's own `Requires.private` reaches nettle/hogweed/libtasn1 in
    # turn — missing any ONE of their pkgconfig dirs makes the whole chain report "not found"
    # with no further explanation (confirmed: curl's configure said plainly "could not find
    # ngtcp2_crypto_gnutls pkg-config file" when only ngtcp2's and gnutls's own dirs were listed;
    # the actual `.pc` file was right there — it was gnutls's OWN transitive deps failing to
    # resolve one level further in).
    # `LIBS="-lhogweed -lnettle -ltasn1 -lgmp"` (bare names, via the shim) is needed for the SAME
    # reason it was needed inside gnutls's own build: this NDK's linker requires every symbol a
    # shared library's own NEEDED entries would satisfy to ALSO resolve explicitly at THIS link
    # step — it does not trust that a `-lgnutls` reference's own transitive NEEDED chain will be
    # satisfied at runtime the way an ordinary ELF loader does; `-z defs`-equivalent strictness,
    # by default, on every link this toolchain performs. Exact-name `-l:lib*.so.N` syntax is
    # unnecessary here (unlike GMP's own case) because nothing else on this system's search path
    # could plausibly satisfy `-lhogweed`/`-lnettle`/`-ltasn1`/`-lgmp` — only our shim does.
    # `--without-ca-bundle --without-ca-path`, no `--with-ca-fallback`: that flag is
    # OpenSSL-specific (`configure: error: --with-ca-fallback only works with OpenSSL`) and is
    # moot regardless — `net.rs` always sets `CURLOPT_CAINFO` explicitly at runtime (see its
    # `apply_roots`/`shipped_ca_bundle`), never relying on curl's own compiled-in default.
    local version archive src build prefix shim
    version=$(python3 "$DEPS" get curl version)
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get curl archive)"
    python3 "$DEPS" fetch curl >/dev/null
    src="$ROOT/vendor/curl-src"; build="$ROOT/vendor/curl-build"; prefix="$ROOT/vendor/curl-prefix"
    shim="$ROOT/vendor/curl-link-shim"
    rm -rf "$src" "$build" "$prefix" "$shim"
    mkdir -p "$src" "$build" "$shim"
    tar xJf "$archive" --strip-components=1 -C "$src"
    # Every dependency was already renamed to its `-plx` SONAME. The shim directory holds a
    # same-basename symlink to each so `-lfoo` resolves at link time while the NEEDED entry `ld`
    # records is the TARGET's own (already renamed) DT_SONAME — no post-link patchelf needed here.
    ln -sf "$ROOT/vendor/gnutls-prefix/lib/libgnutls-plx.so.30" "$shim/libgnutls.so"
    ln -sf "$ROOT/vendor/nettle-prefix/lib/libnettle-plx.so.8" "$shim/libnettle.so"
    ln -sf "$ROOT/vendor/nettle-prefix/lib/libhogweed-plx.so.6" "$shim/libhogweed.so"
    ln -sf "$ROOT/vendor/libtasn1-prefix/lib/libtasn1-plx.so.6" "$shim/libtasn1.so"
    ln -sf "$ROOT/vendor/gmp-prefix/lib/libgmp-plx.so.10" "$shim/libgmp.so"
    ln -sf "$ROOT/vendor/nghttp2-prefix/lib/libnghttp2-plx.so.14" "$shim/libnghttp2.so"
    ln -sf "$ROOT/vendor/nghttp3-prefix/lib/libnghttp3-plx.so.9" "$shim/libnghttp3.so"
    ln -sf "$ROOT/vendor/ngtcp2-prefix/lib/libngtcp2-plx.so.16" "$shim/libngtcp2.so"
    ln -sf "$ROOT/vendor/ngtcp2-prefix/lib/libngtcp2_crypto_gnutls-plx.so.8" "$shim/libngtcp2_crypto_gnutls.so"
    (
        cd "$build"
        export PKG_CONFIG_PATH="$ROOT/vendor/ngtcp2-prefix/lib/pkgconfig:$ROOT/vendor/gnutls-prefix/lib/pkgconfig:$ROOT/vendor/nettle-prefix/lib/pkgconfig:$ROOT/vendor/libtasn1-prefix/lib/pkgconfig:$ROOT/vendor/gmp-prefix/lib/pkgconfig:$ROOT/vendor/nghttp2-prefix/lib/pkgconfig:$ROOT/vendor/nghttp3-prefix/lib/pkgconfig"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 -ffunction-sections -fdata-sections $FILEMAP -I$ROOT/vendor/gnutls-prefix/include -I$ROOT/vendor/nghttp2-prefix/include -I$ROOT/vendor/nghttp3-prefix/include -I$ROOT/vendor/ngtcp2-prefix/include" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT -Wl,--gc-sections -L$shim" \
            LIBS="-lhogweed -lnettle -ltasn1 -lgmp" \
            --with-gnutls="$ROOT/vendor/gnutls-prefix" \
            --with-nghttp2="$ROOT/vendor/nghttp2-prefix" --with-nghttp3="$ROOT/vendor/nghttp3-prefix" \
            --with-ngtcp2="$ROOT/vendor/ngtcp2-prefix" \
            --disable-static --enable-shared \
            --disable-ldap --disable-ldaps --disable-rtsp --disable-telnet --disable-dict \
            --disable-gopher --disable-smtp --disable-pop3 --disable-imap --disable-tftp \
            --disable-mqtt --disable-manual --disable-docs \
            --without-libssh2 --without-libidn2 --without-libpsl \
            --without-brotli --without-zstd --without-gssapi --without-libgsasl \
            --without-ca-bundle --without-ca-path
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install
    )
    ( cd "$prefix/lib"
      real=$(ls libcurl.so.4.* 2>/dev/null | head -1); [ -z "$real" ] && real=libcurl.so.4
      find_evidence_by_content "$real" "$build"
      rename_soname "$real" libcurl-plx.so.4
      scrub_buildpath libcurl-plx.so.4
    )
}

# ---- the keyed output cache (see the header) -----------------------------------------------------
PREFIXES="gmp nettle libtasn1 gnutls nghttp2 nghttp3 ngtcp2 curl"
CACHE_ROOT=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
if [ -n "$CACHE_ROOT" ]; then
    mkdir -p "$CACHE_ROOT"
    CACHE_ROOT=$(cd "$CACHE_ROOT" && pwd)
    PINS=""
    for id in $PREFIXES; do
        PINS="$PINS|$id:$(python3 "$DEPS" get "$id" version):$(python3 "$DEPS" get "$id" sha256)"
    done
    # The same toolchain fingerprint `ci/build-ffmpeg.sh` takes, for the same reasons (its comment
    # has them): what the compiler emits, its banner, binutils', and the size-and-mtime of the
    # files behind the NDK's one shared wrapper, which a reinstall at the same path changes.
    CROSS="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-"
    TOOLCHAIN="$("${CROSS}gcc" -dumpmachine 2>/dev/null)|$("${CROSS}gcc" --version 2>/dev/null | head -1)|$("${CROSS}ld" --version 2>/dev/null | head -1)|$(ls -lnL "${CROSS}gcc.br_real" "${CROSS}ld" "$SYSROOT/lib/libc.so.6" 2>/dev/null | awk '{print $5,$6,$7,$8}' | tr '\n' ' ')"
    SCRIPTS="$(shasum -a 256 "$0" "$ROOT/ci/arm-cc.py" "$ROOT/ci/check-link-evidence.py" | cut -d' ' -f1 | tr '\n' ' ')"
    INHERITED="${CFLAGS-}|${CPPFLAGS-}|${LDFLAGS-}|${CXXFLAGS-}|${PKG_CONFIG_PATH-}|$("$PATCHELF" --version 2>/dev/null)|$("$CMAKE" --version 2>/dev/null | head -1)"
    KEY=$(printf '%s|%s|%s|%s|%s|%s' "$ROOT" "$PINS" "$SYSROOT" "$TOOLCHAIN" "$SCRIPTS" "$INHERITED" \
          | shasum -a 256 | cut -c1-16)
    WORK="$CACHE_ROOT/curlstack/arm-$KEY"
    if [ -f "$WORK/.complete" ]; then
        echo "build-curlstack: cache hit ($WORK) — re-installing the prefixes"
        for p in $PREFIXES; do
            rm -rf "$ROOT/vendor/$p-prefix"
            cp -R "$WORK/$p-prefix" "$ROOT/vendor/$p-prefix"
        done
        : > "$WORK/.last-used"
        echo "build-curlstack: done (cached) — see vendor/{$(echo "$PREFIXES" | tr ' ' ',')}-prefix/lib"
        exit 0
    fi
fi

build_gmp
build_nettle
build_libtasn1
build_gnutls
build_nghttp2
build_nghttp3
build_ngtcp2
build_curl

# Publish whole or not at all: copied to a pid-unique staging directory, marked complete, then
# renamed into place, so a reader never sees a half-written tree. Two builds racing to the same key
# both build; the loser's rename fails and it discards its copy, which is identical by construction.
if [ -n "$CACHE_ROOT" ]; then
    stage="$WORK.stage.$$"
    rm -rf "$stage"
    mkdir -p "$stage"
    for p in $PREFIXES; do
        cp -R "$ROOT/vendor/$p-prefix" "$stage/$p-prefix"
    done
    : > "$stage/.complete"
    : > "$stage/.last-used"
    if [ ! -e "$WORK" ] && mv "$stage" "$WORK" 2>/dev/null; then
        echo "build-curlstack: cached as $WORK"
    else
        rm -rf "$stage"
    fi
fi

echo "build-curlstack: done — see vendor/{gmp,nettle,libtasn1,gnutls,nghttp2,nghttp3,ngtcp2,curl}-prefix/lib"
