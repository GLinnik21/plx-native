#!/usr/bin/env bash
# Build the pinned Sentry Native out-of-process crash handler for the webOS ARM target.
#
# The SDK's own HTTP transport is deliberately disabled. The crash daemon writes a self-contained
# event envelope, relaunches plxnative in its tiny spool-only mode, and the existing consent-aware
# Rust sender posts it on the next healthy boot. That keeps one TLS/libcurl implementation and one
# retry queue in the application.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=0.16.5
ARCHIVE="$ROOT/vendor/sentry-native-$VERSION.tar.gz"
SOURCE="$ROOT/vendor/sentry-native-src"
BUILD="$ROOT/vendor/sentry-native-build"
PREFIX="$ROOT/vendor/sentry-native-prefix"
PATCH="$ROOT/vendor/sentry-native/webos-arm32.patch"
# Two hunks of that patch are upstream PRs and drop out of it once they land in a release we pin:
# pointer-width stack reads (getsentry/sentry-native#2052) and the ARM32 registers + both
# frame-record shapes (#2053). The rest is webOS-only and stays.
URL="https://github.com/getsentry/sentry-native/archive/refs/tags/$VERSION.tar.gz"
SHA256=8d3f63f092ab24ab7f5d30cd8f0e80dc78670a3b3be3f1237948667907cdc3a4

WEBOS_SDK=${WEBOS_SDK:-"$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot"}
CC="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-gcc"
AR="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-ar"
RANLIB="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-ranlib"
STRIP="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-strip"
SYSROOT="$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot"
CMAKE=${CMAKE:-cmake}

fail() { echo "build-sentry-native: $*" >&2; exit 1; }
command -v "$CMAKE" >/dev/null 2>&1 || fail "cmake is required (brew install cmake)"
test -x "$CC" || fail "webOS NDK not found at $WEBOS_SDK (run make setup-env)"
test -f "$PATCH" || fail "missing $PATCH"

if [[ ! -f "$ARCHIVE" ]]; then
    curl -fL --retry 3 --retry-all-errors -o "$ARCHIVE.tmp" "$URL"
    mv "$ARCHIVE.tmp" "$ARCHIVE"
fi
if command -v sha256sum >/dev/null 2>&1; then
    echo "$SHA256  $ARCHIVE" | sha256sum -c -
else
    echo "$SHA256  $ARCHIVE" | shasum -a 256 -c -
fi

# A failed or interrupted rebuild must not leave an old handler looking current.
rm -rf "$SOURCE" "$BUILD" "$PREFIX"
mkdir -p "$SOURCE" "$BUILD" "$PREFIX/bin" "$PREFIX/include" "$PREFIX/lib"
tar xzf "$ARCHIVE" --strip-components=1 -C "$SOURCE"
patch -d "$SOURCE" -p1 < "$PATCH"

# CMAKE_ASM_COMPILER is set explicitly, and it is load-bearing rather than tidy. libunwind's ARM32
# sources are `.S`, so CMake enables the ASM language for them; with no compiler named for that
# language it falls back to a host default. On the aarch64 Linux runner CI builds on, that default
# is the HOST assembler, and `vendor/libunwind/src/arm/getcontext.S` fails with "unknown pseudo-op
# `.arm'" and "unknown mnemonic `stmfd'" — 22 errors, then exit 2. It happened to resolve to the
# cross compiler on this Mac, so the entire cross-build was resting on an implicit default that
# differs by host and CMake version, and CI was red for it while every local build was green.
"$CMAKE" -S "$SOURCE" -B "$BUILD" \
    -DCMAKE_SYSTEM_NAME=Linux \
    -DCMAKE_SYSTEM_PROCESSOR=arm \
    -DCMAKE_C_COMPILER="$CC" \
    -DCMAKE_ASM_COMPILER="$CC" \
    -DCMAKE_AR="$AR" \
    -DCMAKE_RANLIB="$RANLIB" \
    -DCMAKE_SYSROOT="$SYSROOT" \
    -DCMAKE_FIND_ROOT_PATH="$SYSROOT" \
    -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER \
    -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY \
    -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY \
    -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_FLAGS="-funwind-tables -fno-omit-frame-pointer" \
    -DSENTRY_BACKEND=native \
    -DSENTRY_TRANSPORT=none \
    -DSENTRY_BUILD_SHARED_LIBS=OFF \
    -DSENTRY_BUILD_TESTS=OFF \
    -DSENTRY_BUILD_EXAMPLES=OFF \
    -DSENTRY_SDK_NAME=plxnative
"$CMAKE" --build "$BUILD" --parallel "${JOBS:-8}"

cp "$SOURCE/include/sentry.h" "$PREFIX/include/sentry.h"
cp "$BUILD/libsentry.a" "$PREFIX/lib/libsentry.a"
cp "$BUILD/vendor/libunwind/libunwind.a" "$PREFIX/lib/libunwind.a"
# The live developer sampler links the ptrace accessors as a standalone helper.  Keep this archive
# beside the local unwinder rather than reaching into the disposable CMake build tree; neither one
# is packaged unless an explicit package input names it (and only sentry-crash is such an input).
cp "$BUILD/vendor/libunwind/libunwind_remote.a" "$PREFIX/lib/libunwind_remote.a"
cp "$BUILD/sentry-crash" "$PREFIX/bin/sentry-crash"
"$STRIP" --strip-unneeded "$PREFIX/bin/sentry-crash"
chmod 755 "$PREFIX/bin/sentry-crash"

test -s "$PREFIX/lib/libsentry.a"
test -s "$PREFIX/lib/libunwind.a"
test -s "$PREFIX/lib/libunwind_remote.a"
test -x "$PREFIX/bin/sentry-crash"
echo "sentry-native: $VERSION ARM handler ($("$STRIP" --version | head -1))"
du -h "$PREFIX/bin/sentry-crash" "$PREFIX/lib/libsentry.a" "$PREFIX/lib/libunwind.a" \
    "$PREFIX/lib/libunwind_remote.a"
