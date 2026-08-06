#!/bin/sh
# Print the FFmpeg ABI table for the TARGET architecture, from a set of headers.
#
# Needed only when the bundled FFmpeg version changes: run it, paste the numbers into ff.rs's
# constants, then let ci/ffabi-assert.c hold them in place. See ci/ffabi-dump.c for how it
# recovers compile-time constants without executing anything on ARM.
#
#   tools/ffabi-dump.sh                          # the bundled headers (vendor/ffmpeg-prefix)
#   tools/ffabi-dump.sh vendor/ffmpeg-3.3-headers  # any other tree, for comparison
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
CC="$NDK/bin/arm-webos-linux-gnueabi-gcc"
NM="$NDK/bin/arm-webos-linux-gnueabi-nm"
INC=${1:-$ROOT/vendor/ffmpeg-prefix/include}

[ -d "$INC" ] || { echo "no headers at $INC — run 'make ffmpeg' first" >&2; exit 1; }
OBJ=$(mktemp -t ffabi).o
trap 'rm -f "$OBJ"' EXIT

"$CC" --sysroot="$NDK/arm-webos-linux-gnueabi/sysroot" -I "$INC" -std=c11 \
      -c "$ROOT/ci/ffabi-dump.c" -o "$OBJ"

# nm -S prints "<value> <size> <type> <name>"; the size is our datum, hex, stored plus one.
# BSD awk (macOS) has no strtonum, so the hex-to-decimal step goes through the shell.
"$NM" -S --defined-only "$OBJ" | while read -r _addr size type name; do
  case "$name" in plx_*) ;; *) continue ;; esac
  case "$type" in B|b|D|d|C) ;; *) continue ;; esac
  v=$(( 0x$size - 1 ))
  printf '%-24s %8d   0x%x\n' "${name#plx_}" "$v" "$v"
done | sort
