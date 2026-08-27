#!/bin/sh
# Print the FFmpeg ABI table for the TARGET architecture, from a set of headers.
#
# Needed only when the bundled FFmpeg version changes: run it, paste the numbers into ff.rs's
# constants, then let ci/ffabi-assert.c hold them in place. See ci/ffabi-dump.c for how it
# recovers compile-time constants without executing anything on ARM.
#
#   tools/ffabi-dump.sh                          # the bundled headers (vendor/ffmpeg-prefix)
#   tools/ffabi-dump.sh vendor/ffmpeg-3.3-headers  # any other tree, for comparison
#   HOST=1 tools/ffabi-dump.sh                     # THIS MAC, for the simulator's own table
#
# `HOST=1` uses the system compiler against `vendor/ffmpeg-prefix-host/include` and prints the
# 64-bit table. Both tables are then held in place by ci/ffabi-assert.c, which `#if`s on the
# pointer width -- the numbers differ because pointers do, not because the FFmpeg does.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
if [ -n "${HOST:-}" ]; then
  CC=${CC:-cc}
  NM=${NM:-nm}
  DEFAULT_INC=$ROOT/vendor/ffmpeg-prefix-host/include
  SYSROOT_FLAG=
else
  CC="$NDK/bin/arm-webos-linux-gnueabi-gcc"
  NM="$NDK/bin/arm-webos-linux-gnueabi-nm"
  DEFAULT_INC=$ROOT/vendor/ffmpeg-prefix/include
  SYSROOT_FLAG=--sysroot=$NDK/arm-webos-linux-gnueabi/sysroot
fi
INC=${1:-$DEFAULT_INC}

[ -d "$INC" ] || { echo "no headers at $INC — run 'make' first (the FFmpeg build is a prerequisite of the staticlib)" >&2; exit 1; }
OBJ=$(mktemp -t ffabi).o
trap 'rm -f "$OBJ"' EXIT

# The host path COMPILES AND RUNS the same list instead of reading array sizes out of the symbol
# table: macOS `nm` reports every Mach-O size as zero ("sizes with --print-size for Mach-O files
# are always zero"), so the cross trick reads back nothing at all and does so silently.
if [ -n "${HOST:-}" ]; then
  "$CC" -I "$INC" -std=c11 -DPLX_FFABI_MAIN "$ROOT/ci/ffabi-dump.c" -o "$OBJ"
  "$OBJ"
  exit 0
fi

"$CC" $SYSROOT_FLAG -I "$INC" -std=c11 -c "$ROOT/ci/ffabi-dump.c" -o "$OBJ"

# nm -S prints "<value> <size> <type> <name>"; the size is our datum, hex, stored plus one.
# BSD awk (macOS) has no strtonum, so the hex-to-decimal step goes through the shell.
"$NM" -S --defined-only "$OBJ" | while read -r _addr size type name; do
  case "$name" in plx_*) ;; *) continue ;; esac
  case "$type" in B|b|D|d|C) ;; *) continue ;; esac
  v=$(( 0x$size - 1 ))
  printf '%-24s %8d   0x%x\n' "${name#plx_}" "$v" "$v"
done | sort
