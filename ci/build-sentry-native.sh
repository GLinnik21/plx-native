#!/usr/bin/env bash
# Build the pinned Sentry Native out-of-process crash handler for the webOS ARM target.
#
# The SDK's own HTTP transport is deliberately disabled. The crash daemon writes a self-contained
# event envelope, relaunches plxnative in its tiny spool-only mode, and the existing consent-aware
# Rust sender posts it on the next healthy boot. That keeps one TLS/libcurl implementation and one
# retry queue in the application.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=0.13.9
ARCHIVE="$ROOT/vendor/sentry-native-$VERSION.tar.gz"
SOURCE="$ROOT/vendor/sentry-native-src"
BUILD="$ROOT/vendor/sentry-native-build"
PREFIX="$ROOT/vendor/sentry-native-prefix"
PATCH="$ROOT/vendor/sentry-native/webos-arm32.patch"
URL="https://github.com/getsentry/sentry-native/archive/refs/tags/$VERSION.tar.gz"
SHA256=d43a41197ffaa218ceaef8cfcc7ecf584ca1a2c5bda2426b7ab4032875c67167

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

"$CMAKE" -S "$SOURCE" -B "$BUILD" \
    -DCMAKE_SYSTEM_NAME=Linux \
    -DCMAKE_SYSTEM_PROCESSOR=arm \
    -DCMAKE_C_COMPILER="$CC" \
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
cp "$BUILD/sentry-crash" "$PREFIX/bin/sentry-crash"
"$STRIP" --strip-unneeded "$PREFIX/bin/sentry-crash"
chmod 755 "$PREFIX/bin/sentry-crash"

test -s "$PREFIX/lib/libsentry.a"
test -s "$PREFIX/lib/libunwind.a"
test -x "$PREFIX/bin/sentry-crash"
echo "sentry-native: $VERSION ARM handler ($("$STRIP" --version | head -1))"
du -h "$PREFIX/bin/sentry-crash" "$PREFIX/lib/libsentry.a" "$PREFIX/lib/libunwind.a"
