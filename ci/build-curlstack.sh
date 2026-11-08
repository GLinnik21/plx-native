#!/bin/sh
# Cross-compile a vendored, HTTP/3-capable libcurl for the webOS ARM target: OpenSSL 3.5 (libcrypto +
# libssl, TLS 1.3 and the QUIC TLS API ngtcp2's `crypto_ossl` glue needs), nghttp2, ngtcp2 (+ that
# glue), nghttp3, then curl itself, linked against all six.
#
# OPT-IN: only `make CURLSTACK=1` runs this (the Makefile's CURLSTACK block), and only that build
# ships the result and compiles the loader for it (cargo feature `http3-curlstack`). A default
# build never reaches this script.
#
# WHY BUNDLE THIS ONE TOO. The television's own libcurl is pinned at 7.53.1 on the oldest firmware
# (`curlio.rs`'s module doc) — HTTP/1.1 and HTTP/2 only, no QUIC. A reverse proxy (e.g. Traefik) in
# front of a server may advertise `http3` on its entrypoint; the television never could speak it.
# Same reasoning as `ci/build-ffmpeg.sh`'s FFmpeg bundle: a system library this old cannot be grown
# a protocol, only replaced, and dlopen-by-absolute-path is what lets a replacement coexist with
# every webOS release this app still supports (`net::load_bundled` falls back to the television's
# own libcurl the moment this bundle is absent or any one of its NEEDED libraries fails to open;
# `net::curlstack` has the loader's side of the story).
#
# THE OUTPUT IS CACHED, keyed like `ci/build-ffmpeg.sh`'s build tree: under
# $PLX_BUILD_CACHE/curlstack/<key> (default ~/.cache/plxnative), where the key covers every input
# that changes a shipped byte (this script, the five pins, the toolchain's fingerprint, arm-cc.py
# and the link-evidence checker it runs, inherited build variables, and the checkout path the
# build trees and `.pc` files name). A hit re-installs the five `vendor/*-prefix` trees and
# builds nothing, which is what lets a manifest edit that does not touch the stack (an FFmpeg bump)
# re-run this script for free, and what CI's `curlstack` cross-build entry caches. A miss builds
# everything, then publishes it; set PLX_BUILD_CACHE= (empty) to skip the cache entirely.
#
# WHY OPENSSL, AND HOW IT COEXISTS WITH THE TELEVISION'S. OpenSSL's `linux-armv4` perlasm carries
# ARMv8 Crypto Extension paths (AES, PMULL/GHASH, SHA-1, SHA-256) for AArch32, dispatched at run
# time: on a set with a Cortex-A55 that is the difference between table AES and the hardware's own
# instructions for every TLS byte. The television's own OpenSSL (1.0.x to 3.x by firmware) is in the
# process too, brought in by the system libcurl that `libplayerAPIs.so.1` (every Feed build links
# it) needs, so this one is
# kept apart from it at every level: its own SONAMEs and symbol-version file names
# (`shlib_variant`, build_openssl), no configuration or module directory any television has
# (build_openssl), and every library opened `RTLD_DEEPBIND | RTLD_LOCAL` (`net::curlstack`'s
# module doc), so the bundle binds its own internal calls to itself and none of it joins the
# global scope.
#
# THE SONAME-RENAME CONVENTION is the same `-plx` infix FFmpeg uses (`libavformat-plx.so.63`,
# ...): `libcrypto-plx.so.3`, `libssl-plx.so.3`, `libnghttp2-plx.so.14`, `libnghttp3-plx.so.9`,
# `libngtcp2-plx.so.16`, `libngtcp2_crypto_ossl-plx.so.0`, `libcurl-plx.so.4`. A renamed SONAME is not cosmetic:
# `net::curlstack` dlopens these by ABSOLUTE PATH beside the binary (no rpath survives
# FFmpeg's own configure-eval problem, and these build systems have the identical issue), so the
# bare name must never collide with anything the device itself might already carry.
#
# NO COMPILE-TIME NEON, EVER. The oldest supported device is a real ARM Cortex-A9
# (docs/measurements/softfloat-arm-tv-2026-09-07.md), and this project has a documented SIGILL
# incident from targeting newer ARM codegen (docs/simplification-audit-2026-07-18.md:
# `-C target-cpu=cortex-a53` SIGILLs on this SoC via a missing/different CP15 barrier). The NDK
# gcc's default target (cortex-a9/armv7-a/soft-float, Makefile:263) is exactly right for every one
# of these builds with ZERO `-march=`/`-mcpu=`/`-mfpu=` flags added — do not add them. Any
# hand-written assembly here must be plain ARMv4/v7 integer code or runtime-dispatched
# integer routines, not a CPU-specific variant. OpenSSL's `linux-armv4` perlasm is assembled with
# `.fpu neon`/ARMv8 instruction encodings, but every one of those paths is selected at RUN TIME from
# `OPENSSL_armcap_P` (`crypto/armcap.c`), so the same library runs table code on a Cortex-A9 and the
# Crypto Extensions on an A55 — not a compile-time ISA assumption.
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
# None of these seven build with -g explicitly, yet every output below carries debug_info and
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

build_openssl() {
    # `linux-armv4`, NOT `linux-generic32` (which implies `no-asm`): the armv4 target is OpenSSL's
    # real 32-bit ARM port, and its perlasm (aesv8-armx, ghashv8-armx, sha1/sha256-armv4,
    # chacha/poly1305-armv4, bsaes-armv7) is what carries the ARMv8 Crypto Extension and NEON paths.
    # Each is gated at run time by `OPENSSL_armcap_P`. HOW that is filled matters on this sysroot:
    # `crypto/armcap.c` uses `getauxval(AT_HWCAP/AT_HWCAP2)` only when `__GLIBC_PREREQ(2, 16)`, and
    # the NDK's glibc is 2.12, so it takes its third route — executing one probe instruction per
    # feature under a temporary SIGILL handler (`sigsetjmp`), once, in the library's init. That
    # route needs no glibc symbol the oldest firmware lacks, and it sees exactly what the CPU
    # executes in AArch32 state, which on a 64-bit kernel is what the compat HWCAP2 bits describe.
    #
    # THE `-plx` NAME IS OpenSSL's OWN `shlib_variant`, not a post-link patchelf. OpenSSL's shared
    # libraries carry symbol versions (`OPENSSL_3.0.0`, ...) whose `.gnu.version_r` entries name the
    # FILE that defines them; a renamed SONAME with a stale `libcrypto.so.3` there would let the
    # loader match libssl's version needs against the FIRMWARE's libcrypto.so.3 (webOS 10 ships
    # 3.0.x). A small config target that inherits `linux-armv4` and sets `shlib_variant => "-plx"`
    # makes the build itself emit `libcrypto-plx.so.3`/`libssl-plx.so.3`, so every SONAME, NEEDED
    # and version-needs entry is right from the link.
    #
    # NO CONFIG FILE, NO MODULE DIRECTORY, ANYWHERE ON THE DEVICE. curl calls
    # `OPENSSL_init_ssl(OPENSSL_INIT_LOAD_CONFIG)`, which reads `$OPENSSLDIR/openssl.cnf`; with the
    # firmware's `/etc/ssl` that would be the television's own OpenSSL configuration, written for a
    # different major. So `--openssldir`/`--prefix` name a path that exists on no television
    # (`/nonexistent/plx-openssl`; a missing default config is silently skipped), the install goes
    # through DESTDIR, and the `.pc` files are pointed back at the real tree afterwards.
    # `no-module` builds the default provider INTO libcrypto (nothing is ever looked up under
    # MODULESDIR), `no-legacy` drops the legacy provider, `no-engine` the engines directory and
    # `no-autoload-config` libssl's implicit config load. TRUST ANCHORS are not OpenSSL's business
    # here: curl is built `--with-ca-path=/etc/ssl/certs` (build_curl), the television's hashed
    # directory, never its stale concatenated bundle.
    #
    # `-Wl,--as-needed`: `linux-armv4` inherits `linux-latomic`, which appends `-latomic` for
    # 64-bit atomics on 32-bit targets. GCC inlines those as `ldrexd`/`strexd` for ARMv7, so
    # libcrypto imports nothing from libatomic, and an unconditional `NEEDED libatomic.so.1` is a
    # library webOS 1.2, 1.4, 4.4.2 and 4.10.0 do not have (`tools/fwcompat.py`).
    local archive src build prefix stage fake
    fake=/nonexistent/plx-openssl
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get openssl archive)"
    python3 "$DEPS" fetch openssl >/dev/null
    src="$ROOT/vendor/openssl-src"; build="$ROOT/vendor/openssl-build"; prefix="$ROOT/vendor/openssl-prefix"
    stage="$ROOT/vendor/openssl-stage"
    rm -rf "$src" "$build" "$prefix" "$stage"; mkdir -p "$src" "$build" "$stage"
    tar xzf "$archive" --strip-components=1 -C "$src"
    cat > "$build/plx-target.conf" <<'CONF'
my %targets = (
    "linux-armv4-plx" => {
        inherit_from  => [ "linux-armv4" ],
        shlib_variant => "-plx",
    },
);
CONF
    (
        cd "$build"
        # NO --cross-compile-prefix: Configure prepends it to CC, and CC is already a full path.
        perl "$src/Configure" --config="$build/plx-target.conf" linux-armv4-plx shared \
            "CC=$CC" "AR=$AR" "RANLIB=$RANLIB" \
            "CFLAGS=--sysroot=$SYSROOT -O3 -Wall -ffunction-sections -fdata-sections $FILEMAP" \
            "LDFLAGS=--sysroot=$SYSROOT -Wl,--gc-sections -Wl,--as-needed" \
            --prefix="$fake" --openssldir="$fake" --libdir=lib \
            no-module no-legacy no-engine no-autoload-config no-deprecated no-comp \
            no-idea no-camellia no-seed no-md2 no-mdc2 no-rc5 no-whirlpool no-srp no-ssl3 \
            no-weak-ssl-ciphers no-apps no-docs no-tests no-fuzz-afl no-fuzz-libfuzzer
        make -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
        make install_sw DESTDIR="$stage"
    )
    mv "$stage$fake" "$prefix"; rm -rf "$stage"
    # `install_sw` installs the static archives too, and OpenSSL's Configure has no switch to skip
    # them. Nothing links them (ngtcp2 is handed the shared files by name, curl's `-lssl -lcrypto`
    # resolve through the `.so` links below), and left in place they would be 11 MB of the CI
    # cache entry and a static link waiting for the first build that drops a shared name.
    rm -f "$prefix/lib/libcrypto.a" "$prefix/lib/libssl.a"
    sed -i.bak "s|^prefix=.*|prefix=$prefix|; s|^libdir=.*|libdir=$prefix/lib|; s|^includedir=.*|includedir=$prefix/include|" \
        "$prefix"/lib/pkgconfig/*.pc && rm -f "$prefix"/lib/pkgconfig/*.pc.bak
    ( cd "$prefix/lib"
      for lib in libcrypto-plx.so.3 libssl-plx.so.3; do
          test -f "$lib" || fail "OpenSSL did not produce $lib (shlib_variant ignored?)"
          find_evidence_by_content "$lib" "$build"
          scrub_buildpath "$lib"
      done
      # Link-time names for ngtcp2's and curl's `-lssl -lcrypto`; the NEEDED `ld` records is the
      # target's own DT_SONAME (already `-plx`).
      ln -sf libcrypto-plx.so.3 libcrypto.so
      ln -sf libssl-plx.so.3 libssl.so
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
    # `-DENABLE_OPENSSL=ON -DENABLE_GNUTLS=OFF`: with plain OpenSSL >= 3.5 (it has
    # `SSL_set_quic_tls_cbs`, and not quictls' `SSL_provide_quic_data`), ngtcp2's CMake enables the
    # `crypto_ossl` glue and disables quictls on its own. `find_package(OpenSSL)` is fed the CACHE
    # variables directly rather than fighting cross-compiled pkg-config detection.
    # `CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY` is CRITICAL, not optional: without it the
    # `check_symbol_exists` probes try to link a full EXECUTABLE against the cross sysroot, which
    # fails in a way indistinguishable from "OpenSSL genuinely lacks the symbol" — a cross-compile
    # try_compile artifact, the same class of gotcha `ci/build-sentry-native.sh`'s doc describes.
    # ngtcp2's CMakeLists also `enable_language()`s CXX unconditionally (examples/tests exist even
    # with BUILD_TESTING=OFF); left unset, CMake derives a companion CXX from the HOST's own
    # default (`/usr/bin/c++`) rather than the cross toolchain. Point it at the real cross g++
    # directly: nothing here links a C++ object.
    local archive src build prefix ossl
    ossl="$ROOT/vendor/openssl-prefix"
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get ngtcp2 archive)"
    python3 "$DEPS" fetch ngtcp2 >/dev/null
    src="$ROOT/vendor/ngtcp2-src"; build="$ROOT/vendor/ngtcp2-build"; prefix="$ROOT/vendor/ngtcp2-prefix"
    rm -rf "$src" "$build" "$prefix"; mkdir -p "$src"
    tar xzf "$archive" --strip-components=1 -C "$src"
    "$CMAKE" -S "$src" -B "$build" \
        -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=arm \
        -DCMAKE_C_COMPILER="$CC" -DCMAKE_CXX_COMPILER="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-c++" \
        -DCMAKE_AR="$AR" -DCMAKE_RANLIB="$RANLIB" \
        -DCMAKE_SYSROOT="$SYSROOT" -DCMAKE_FIND_ROOT_PATH="$SYSROOT;$ossl" \
        -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY \
        -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY \
        -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
        -DOPENSSL_ROOT_DIR="$ossl" -DOPENSSL_INCLUDE_DIR="$ossl/include" \
        -DOPENSSL_SSL_LIBRARY="$ossl/lib/libssl-plx.so.3" -DOPENSSL_CRYPTO_LIBRARY="$ossl/lib/libcrypto-plx.so.3" \
        -DENABLE_OPENSSL=ON -DENABLE_GNUTLS=OFF -DENABLE_BORINGSSL=OFF -DENABLE_PICOTLS=OFF -DENABLE_WOLFSSL=OFF \
        -DENABLE_STATIC_LIB=OFF -DENABLE_SHARED_LIB=ON -DBUILD_TESTING=OFF \
        -DCMAKE_C_FLAGS="-ffunction-sections -fdata-sections $FILEMAP" \
        -DCMAKE_SHARED_LINKER_FLAGS="-Wl,--gc-sections"
    "$CMAKE" --build "$build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
    "$CMAKE" --install "$build"
    ( cd "$prefix/lib"
      real1=$(ls libngtcp2.so.16.* 2>/dev/null | head -1); [ -z "$real1" ] && real1=libngtcp2.so.16
      real2=$(ls libngtcp2_crypto_ossl.so.0.* 2>/dev/null | head -1); [ -z "$real2" ] && real2=libngtcp2_crypto_ossl.so.0
      test -f "$real2" || fail "ngtcp2 built no crypto_ossl glue (OpenSSL QUIC TLS API not detected)"
      find_evidence_by_content "$real1" "$build"
      find_evidence_by_content "$real2" "$build"
      rename_soname "$real1" libngtcp2-plx.so.16
      rename_soname "$real2" libngtcp2_crypto_ossl-plx.so.0
      repoint_needed libngtcp2_crypto_ossl-plx.so.0 libngtcp2.so.16 libngtcp2-plx.so.16
      scrub_buildpath libngtcp2-plx.so.16
      scrub_buildpath libngtcp2_crypto_ossl-plx.so.0
      ln -sf libngtcp2-plx.so.16 libngtcp2.so
      ln -sf libngtcp2_crypto_ossl-plx.so.0 libngtcp2_crypto_ossl.so
    )
}

build_curl() {
    # `PKG_CONFIG_PATH` spans every prefix: curl's configure resolves `libngtcp2_crypto_ossl`'s
    # pkg-config `Requires.private` (libngtcp2, libssl, libcrypto) transitively, and missing ONE
    # dir makes the whole chain report "not found" with no further explanation.
    # `--with-openssl` + `--with-ngtcp2`, NOT `--with-openssl-quic`: that one is OpenSSL's OWN QUIC
    # stack, mutually exclusive with ngtcp2; with ngtcp2 and OpenSSL >= 3.5 curl's configure picks
    # the `ngtcp2_crypto_ossl` glue (its OPENSSL_QUIC_API2 branch).
    # TRUST: `--with-ca-path=/etc/ssl/certs --without-ca-bundle`, no `--with-ca-fallback`. Under the
    # OpenSSL backend on Linux `CURLSSLOPT_NATIVE_CA` loads nothing (curl's `vtls/openssl.c` honours
    # it only for Windows and Apple stores), and the fallback would mean OpenSSL's built-in default
    # paths, which build_openssl deliberately points at a directory no television has. So the CA
    # PATH is compiled in: the television's hashed `/etc/ssl/certs` (the c_rehash `<hash>.0` links
    # OpenSSL itself reads), which is current even where the concatenated `ca-certificates.crt`
    # beside it is stale (no ISRG Root X1 on webOS 5.6.2). The `*.plex.direct` roots fallback still
    # adds the shipped `le-roots.pem` with `CURLOPT_CAINFO` (`apply_roots`/`shipped_ca_bundle`).
    local version archive src build prefix shim
    version=$(python3 "$DEPS" get curl version)
    archive="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get curl archive)"
    python3 "$DEPS" fetch curl >/dev/null
    src="$ROOT/vendor/curl-src"; build="$ROOT/vendor/curl-build"; prefix="$ROOT/vendor/curl-prefix"
    shim="$ROOT/vendor/curl-link-shim"
    rm -rf "$src" "$build" "$prefix" "$shim"
    mkdir -p "$src" "$build" "$shim"
    tar xJf "$archive" --strip-components=1 -C "$src"
    # Every dependency already carries its `-plx` SONAME. The shim directory holds a same-basename
    # symlink to each so `-lfoo` resolves at link time while the NEEDED entry `ld` records is the
    # TARGET's own DT_SONAME — no post-link patchelf needed here.
    ln -sf "$ROOT/vendor/openssl-prefix/lib/libssl-plx.so.3" "$shim/libssl.so"
    ln -sf "$ROOT/vendor/openssl-prefix/lib/libcrypto-plx.so.3" "$shim/libcrypto.so"
    ln -sf "$ROOT/vendor/nghttp2-prefix/lib/libnghttp2-plx.so.14" "$shim/libnghttp2.so"
    ln -sf "$ROOT/vendor/nghttp3-prefix/lib/libnghttp3-plx.so.9" "$shim/libnghttp3.so"
    ln -sf "$ROOT/vendor/ngtcp2-prefix/lib/libngtcp2-plx.so.16" "$shim/libngtcp2.so"
    ln -sf "$ROOT/vendor/ngtcp2-prefix/lib/libngtcp2_crypto_ossl-plx.so.0" "$shim/libngtcp2_crypto_ossl.so"
    (
        cd "$build"
        export PKG_CONFIG_PATH="$ROOT/vendor/ngtcp2-prefix/lib/pkgconfig:$ROOT/vendor/openssl-prefix/lib/pkgconfig:$ROOT/vendor/nghttp2-prefix/lib/pkgconfig:$ROOT/vendor/nghttp3-prefix/lib/pkgconfig"
        "$src/configure" --host=arm-webos-linux-gnueabi --prefix="$prefix" \
            CC="$CC" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="--sysroot=$SYSROOT -O2 -ffunction-sections -fdata-sections $FILEMAP -I$ROOT/vendor/openssl-prefix/include -I$ROOT/vendor/nghttp2-prefix/include -I$ROOT/vendor/nghttp3-prefix/include -I$ROOT/vendor/ngtcp2-prefix/include" \
            CPPFLAGS="--sysroot=$SYSROOT" \
            LDFLAGS="--sysroot=$SYSROOT -Wl,--gc-sections -L$shim" \
            --with-openssl="$ROOT/vendor/openssl-prefix" \
            --with-nghttp2="$ROOT/vendor/nghttp2-prefix" --with-nghttp3="$ROOT/vendor/nghttp3-prefix" \
            --with-ngtcp2="$ROOT/vendor/ngtcp2-prefix" \
            --disable-static --enable-shared \
            --disable-ldap --disable-ldaps --disable-rtsp --disable-telnet --disable-dict \
            --disable-gopher --disable-smtp --disable-pop3 --disable-imap --disable-tftp \
            --disable-mqtt --disable-manual --disable-docs \
            --without-libssh2 --without-libidn2 --without-libpsl \
            --without-brotli --without-zstd --without-gssapi --without-libgsasl \
            --without-ca-bundle --with-ca-path=/etc/ssl/certs
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
PREFIXES="openssl nghttp2 nghttp3 ngtcp2 curl"
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
        mkdir -p "$ROOT/vendor"
        for p in $PREFIXES; do
            rm -rf "$ROOT/vendor/$p-prefix"
            cp -R "$WORK/$p-prefix" "$ROOT/vendor/$p-prefix"
        done
        : > "$WORK/.last-used"
        echo "build-curlstack: done (cached) — see vendor/{$(echo "$PREFIXES" | tr ' ' ',')}-prefix/lib"
        exit 0
    fi
fi

build_openssl
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

echo "build-curlstack: done — see vendor/{openssl,nghttp2,nghttp3,ngtcp2,curl}-prefix/lib"
