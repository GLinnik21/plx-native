#!/bin/sh
# Print a 16-hex-digit identity for everything a HOST build of FFmpeg or libass reads from the
# MACHINE rather than from the repository, so a CI cache key can name it.
#
# WHY IT EXISTS. `ci/build-ffmpeg.sh` and `ci/build-libass.py` each re-derive a content key for their
# own build tree and rebuild whenever it moves, and that key includes the compiler. A CI cache key
# made only of `hashFiles(...)` cannot see the compiler, so after a runner-image update the cache
# would keep hitting under an unchanged key, the script would (correctly) refuse the restored
# objects and rebuild, and nothing would ever be re-saved: a slow build forever, behind a log that
# says "Cache hit". Putting this value in the key makes a new image a new entry instead.
#
# WHAT IT COVERS, which is the machine-side half of those two keys:
#   * the compiler's target triple and its full version banner (a Rosetta run reports the same
#     banner with a different target, as build-ffmpeg.sh's own comment explains);
#   * the macOS SDK version (a no-op elsewhere), which Xcode moves independently of the banner;
#   * the variables the scripts either hash or forward to configure: CFLAGS, CPPFLAGS, LDFLAGS,
#     CXXFLAGS, PKG_CONFIG_PATH, and RELEASE (it selects FFmpeg's component list);
#   * the checkout's absolute path, which ends up in libass's prefix and its cmake flags and is
#     part of that script's key. It is the same on every run of one runner, which is the point.
#
# The value is hex on purpose: tools/prune-gh-caches.py strips trailing hex segments to find a
# cache's "family" and treats the newest entry of each family as the live generation.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
{
  printf 'machine=%s\n' "$(cc -dumpmachine)"
  printf 'cc=%s\n' "$(cc --version)"
  printf 'sdk=%s\n' "$(xcrun --show-sdk-version 2>/dev/null || true)"
  printf 'env=%s|%s|%s|%s|%s|%s\n' "${CFLAGS-}" "${CPPFLAGS-}" "${LDFLAGS-}" "${CXXFLAGS-}" \
    "${PKG_CONFIG_PATH-}" "${RELEASE-}"
  printf 'root=%s\n' "$ROOT"
} | shasum -a 256 | cut -c1-16
