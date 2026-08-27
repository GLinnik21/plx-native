#!/bin/sh
# Stage the HOST FFmpeg beside pkg/ for the desktop simulator, with loader-relative names.
#
# `ff.rs` dlopens the bundled libraries by ABSOLUTE PATH out of `paths::app_dir()`, which is
# `pkg/` under `make sim` — so the Mach-O build has to sit there next to the ELF one. The two
# cannot be confused: different extension, and `APP_FILES` in the Makefile is an explicit list
# rather than a glob, so nothing here can reach an .ipk or a television.
#
# THE ONE THING THIS SCRIPT IS FOR. FFmpeg's configure records `--prefix` in each dylib's install
# name, and that prefix is the literal `/plx` (see ci/build-ffmpeg.sh — it is a stand-in chosen so
# no build path leaks into the shipped libraries). `/plx/lib/libavutil-plx.61.dylib` does not
# exist, so dyld fails to resolve libavcodec's reference to libavutil and the whole chain refuses
# to load. On Linux the app sidesteps this by opening the three in dependency order with
# RTLD_GLOBAL; that does NOT work on macOS, where dyld resolves by install name rather than
# through a global namespace, so the names must be rewritten. `@loader_path` rather than `@rpath`
# because these are opened by dlopen from an arbitrary executable, which has no rpath to inherit.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PREFIX="$ROOT/vendor/ffmpeg-prefix-host"
LIBS='libavutil-plx.61 libavcodec-plx.63 libavformat-plx.63 libswscale-plx.10'
name=$1

src=$(ls "$PREFIX/lib/$name".*.*.dylib 2>/dev/null | head -1)
[ -n "$src" ] || { echo "no host $name in $PREFIX — run 'HOST=1 ci/build-ffmpeg.sh'" >&2; exit 1; }
out="$ROOT/pkg/$name.dylib"
cp "$src" "$out"
chmod u+w "$out"
install_name_tool -id "@loader_path/$name.dylib" "$out"
for dep in $LIBS; do
  install_name_tool -change "/plx/lib/$dep.dylib" "@loader_path/$dep.dylib" "$out"
done
# Rewriting load commands invalidates the ad-hoc signature Apple silicon requires; without this
# the dlopen fails with "code signature invalid" and nothing says which of the two edits did it.
codesign -f -s - "$out" 2>/dev/null || true
