#!/bin/sh
# Cross-compile the FFmpeg the app SHIPS, for 32-bit ARM, with the webOS NDK.
#
# WHY WE BUNDLE. The app used to demux with the television's own FFmpeg, which meant reading its
# structs at byte offsets we could not check: LG ships no headers, the binaries are stripped, and
# the major moves with the firmware (55 -> 57 -> 58 -> 59 -> 60 across webOS 2 to 11). That was
# survivable while the offsets could be re-derived against upstream headers at the matching
# version — but only for the LAYOUT half of the problem. The other half is not checkable at all:
# demuxers, bitstream filters, encoders and muxers live in a REGISTRY, as data, not as exported
# symbols. No symbol table and no firmware database can answer "does this set's libavcodec have
# h264_mp4toannexb", and the author has no hardware past webOS 4.5 to ask. Bundling turns both
# halves into compile-time facts.
#
# It also collapses the maintenance: one FFmpeg on every television, so adding a webOS release
# costs nothing and the demuxer stops being a variable in every bug report.
#
# THIS IS ALSO WEBOSBREW'S PUBLISHED GUIDANCE, which is worth knowing before anyone re-opens the
# decision. https://www.webosbrew.org/develop/caniuse/?q=ffmpeg carries a warning rather than a
# compatibility table: "Don't use system FFmpeg libraries! They will cause linkage issues and
# doesn't come with usable video codecs either." Both halves are the reasoning above — the SONAME
# drift, and the component list that cannot be inspected from outside.
#
# LICENCE. Built SHARED, deliberately. FFmpeg here is LGPL-2.1 (no --enable-gpl, no --enable-
# nonfree), and shipping it as separate .so files keeps us in §6(b) — the user can replace the
# library — exactly as dynamic linking against the TV's copy did. Static linking would pull in
# §6(a)'s relinkable-object obligation for no benefit. The source tarball this builds from is
# published beside each release; see THIRD-PARTY-NOTICES.md.
#
# BUILD SUFFIX. The libraries are named libavformat-plx.so.60 and so on. webOS 11.2.0 ships
# FFmpeg 6 too, so without the suffix our file names and SONAMEs would collide with the
# television's own — and "which libavformat did we actually get" is precisely the question that
# cannot be answered remotely. With it, the answer is structural.
#
# NO RPATH, DELIBERATELY. The obvious way to make our libavformat find our libavcodec is
# -Wl,-rpath,$ORIGIN, and it does not survive: FFmpeg's configure evals its flags, so `$$ORIGIN`
# becomes the shell's PID and `\$$ORIGIN` becomes nothing. Both produce a library that silently
# loads a DIFFERENT libavcodec. Instead ff.rs dlopens the four in dependency order with
# RTLD_GLOBAL — libavutil, then libavcodec, then libavformat — so each is already in the global
# scope by SONAME when the next one names it. That is ordinary loader behaviour, it needs no build
# flag to survive quoting, and unlike an rpath it can be asserted from inside the app.
#
# HOST=1 BUILDS THE SAME FFMPEG FOR THIS MAC INSTEAD, and it is the same script on purpose.
#
# The desktop simulator (`make sim`) runs the real app core on macOS, but `ff.rs` dlopens the
# bundled libraries by absolute path out of the app directory — where they are 32-bit ARM ELF. So
# the ENTIRE streaming half of the app (the AVIO transports, the HLS demux, the AU queues and
# therefore the whole adaptive controller) was device-only, and the one instrument that can run
# them without a television could not reach them: `ff: FFmpeg unavailable — the app runs, playback
# will refuse`, measured 2026-08-28.
#
# **One script, and specifically ONE COMPONENT LIST.** Everything below `set --` is shared: same
# version, same demuxers, parsers, bitstream filters and decoders, same `--disable-network` with
# `file` as the only protocol, same `-plx` suffix, same LGPL flags. A second script would drift,
# and the drift would be invisible — the host would demux something the television cannot, or
# stop demuxing something it can, and the simulator would be answering about a different FFmpeg
# while looking identical. The cross block is the only difference, and it is three lines.
#
# The output is `vendor/ffmpeg-prefix-host` and `libavformat-plx.63.dylib` — Mach-O naming, so
# nothing here can be confused with the shipped ELF even by basename, and `make ipk`'s
# `lib*-plx.so.*` glob cannot pick it up.
set -eu

VERSION=9.0
SHA256=7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
SYSROOT="$NDK/arm-webos-linux-gnueabi/sysroot"
HOST=${HOST:-}
if [ -n "$HOST" ]; then
  WORK="$ROOT/vendor/ffmpeg-build-host"
  PREFIX="$ROOT/vendor/ffmpeg-prefix-host"
else
  WORK="$ROOT/vendor/ffmpeg-build"
  PREFIX="$ROOT/vendor/ffmpeg-prefix"
fi
# `--prefix` is the LITERAL /plx, not $PREFIX, and the install is redirected with DESTDIR below.
#
# WHY: FFmpeg records its entire configure INVOCATION inside libavutil (`avutil_configuration()`),
# so every absolute path on that command line ends up in the shipped `.so`. A real `--prefix` put
# the maintainer's working directory in all three libraries, and with it the `.ipk` sha — which
# silently made "builds are reproducible" false in the v0.2.0 and v0.2.1 release notes, on the one
# number a user has to check an unsigned download. `/plx` is a stand-in that exists nowhere.
#
# It does NOT make a local build reproducible, and nothing here can: `--cross-prefix` must be
# absolute (see below — the NDK gcc is a wrapper that dies when invoked through PATH), so the
# configure string still carries wherever the NDK lives. Reproducibility is therefore a property
# of the CI BUILD, whose path is fixed for every GitHub runner — which is the real reason releases
# must be published by the workflow and never by hand. Do not restore a "rebuild and compare"
# claim to the notes without checking `strings` on all three libraries first.
#
# Do NOT "fix" this with -ffile-prefix-map: that flag is itself part of the configure line, so
# passing absolute paths to it ADDS two more leaks than it removes. Tried, measured, reverted.
SRC="$WORK/ffmpeg-$VERSION"

# The NDK's gcc is a WRAPPER that resolves its own path at startup. Invoked through PATH it dies
# with "toolchain-wrapper.c: readlink: No such file or directory" and configure reports the
# misleading "C compiler test failed", so --cross-prefix must be absolute.
CROSS="$NDK/bin/arm-webos-linux-gnueabi-"

if [ -z "$HOST" ]; then
  [ -x "${CROSS}gcc" ] || { echo "ffmpeg: no NDK at $NDK — run 'make setup-env'" >&2; exit 1; }
fi

mkdir -p "$WORK"
TAR="$WORK/ffmpeg-$VERSION.tar.xz"
if [ ! -f "$TAR" ]; then
  echo "ffmpeg: fetching $VERSION"
  curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$TAR.part"
  mv "$TAR.part" "$TAR"
fi
# Verify before extracting: this tarball becomes code inside the shipped package.
have=$(shasum -a 256 "$TAR" | cut -d' ' -f1)
if [ "$have" != "$SHA256" ]; then
  echo "ffmpeg: SHA256 MISMATCH for $TAR" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $have" >&2
  exit 1
fi
[ -d "$SRC" ] || tar xf "$TAR" -C "$WORK"

# Wipe the prefix whenever the version changes. `make install` only ADDS files, so a version bump
# leaves the previous major's libraries sitting beside the new ones — and since the Makefile stages
# by SONAME glob, the package would quietly ship whichever the shell listed first. Found the hard
# way going 6.1 -> 8.1: the prefix held libavformat 60 AND 62.
STAMP="$PREFIX/.plx-version"
if [ ! -f "$STAMP" ] || [ "$(cat "$STAMP")" != "$VERSION" ]; then
  rm -rf "$PREFIX"
  mkdir -p "$PREFIX"
  printf '%s' "$VERSION" > "$STAMP"
fi

# Components: exactly what the app uses, and nothing else — the size is almost entirely a
# function of this list. Each line maps to real code:
#   demuxers   matroska/mov/mpegts are the containers PMS direct-plays or transcodes to;
#              h264/hevc are the raw Annex-B paths (/tmp/sample.h264 and the dev triggers).
#   parsers    needed by avformat_find_stream_info to fill AVCodecParameters.
#   bsf        AVCC -> Annex-B for mp4-family video, which the Starfish pipeline requires.
#   decoders   SUBTITLES ONLY. Video and audio are decoded by the TV's hardware via Starfish;
#              FFmpeg here never touches a video frame.
#   encoder/   mpeg1video + mpegts are the DEV capture stream only, dropped by RELEASE=1 along
#   muxer      with swscale, which nothing else uses.
if [ -n "$HOST" ]; then
  # No --arch/--cpu/--target-os: configure detects this Mac, which is the point.
  set -- --prefix=/plx
else
  set -- --prefix=/plx \
    --enable-cross-compile --cross-prefix="$CROSS" --host-cc=cc \
    --arch=arm --cpu=cortex-a9 --target-os=linux --sysroot="$SYSROOT"
fi
set -- "$@" \
  --build-suffix=-plx \
  --enable-shared --disable-static --enable-pic \
  --extra-cflags='-Dstatic_assert=_Static_assert' \
  --disable-programs --disable-doc --disable-avdevice --disable-avfilter \
  --disable-network --disable-swresample \
  --disable-debug --enable-small \
  --disable-everything \
  --enable-demuxer=matroska,mov,mpegts,h264,hevc \
  --enable-parser=h264,hevc,aac,ac3,dvdsub,dvbsub \
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,extract_extradata \
  --enable-decoder=pgssub,dvdsub,dvbsub,ass,srt,subrip,text,mov_text,webvtt \
  --enable-protocol=file

if [ "${RELEASE:-}" = "1" ]; then
  set -- "$@" --disable-swscale
else
  set -- "$@" --enable-encoder=mpeg1video --enable-muxer=mpegts
fi

cd "$SRC"
# Reconfigure only when the flags change; FFmpeg's configure is slow and this script is a
# prerequisite of every build.
FLAGS_FILE="$SRC/.plx-flags"
NEW_FLAGS=$*
if [ ! -f "$FLAGS_FILE" ] || [ "$(cat "$FLAGS_FILE")" != "$NEW_FLAGS" ]; then
  echo "ffmpeg: configuring ($VERSION, $([ "${RELEASE:-}" = 1 ] && echo release || echo dev))"
  ./configure "$@" > "$WORK/configure.log" 2>&1 || {
    echo "ffmpeg: CONFIGURE FAILED — $WORK/configure.log" >&2; tail -20 "$WORK/configure.log" >&2; exit 1; }
  printf '%s' "$NEW_FLAGS" > "$FLAGS_FILE"
fi

echo "ffmpeg: building"
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" > "$WORK/build.log" 2>&1 || {
  echo "ffmpeg: BUILD FAILED — $WORK/build.log" >&2; tail -20 "$WORK/build.log" >&2; exit 1; }
# DESTDIR redirects the /plx prefix onto the real tree: the libraries believe they were
# configured for /plx (so they record no build path) and land in $PREFIX regardless.
make install DESTDIR="$WORK/destdir" > "$WORK/install.log" 2>&1
mkdir -p "$PREFIX"
cp -R "$WORK/destdir/plx/." "$PREFIX/"

# Strip: these ship, and the debug info is ~4x the code. Only the shipped build — the host copy
# never leaves this machine, and `strip` on a Mach-O dylib is a different flag set for no gain.
if [ -z "$HOST" ]; then
  for f in "$PREFIX"/lib/lib*-plx.so.*; do
    [ -f "$f" ] && [ ! -L "$f" ] && "${CROSS}strip" --strip-unneeded "$f"
  done
fi

echo "ffmpeg: installed to $PREFIX"
for f in "$PREFIX"/lib/lib*-plx.so.* "$PREFIX"/lib/lib*-plx.*.dylib; do
  [ -f "$f" ] && [ ! -L "$f" ] && printf '  %-28s %6s KB\n' "$(basename "$f")" "$(( $(wc -c < "$f") / 1024 ))"
done
