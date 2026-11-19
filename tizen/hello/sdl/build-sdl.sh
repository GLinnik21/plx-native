#!/usr/bin/env bash
# Build the STATIC SDL2 + SDL2_ttf that the H5 probe links into libplx.so.
#
#   tizen/hello/sdl/build-sdl.sh arm|x86_64|i686|host [--force]
#
# build.sh runs this for you before it builds the library (so `build.sh --arch x86_64` in WSL needs
# nothing else). Run it alone to prebuild, or with `host` for the macOS/Linux unit test
# (`build.sh --host-test`).
#
# Product, per arch (all gitignored, cached; a stamp of the pins + this script + the flags skips a
# rebuild that would change nothing):
#   tizen/hello/sdl-build/<arch>/include/SDL2/*.h
#   tizen/hello/sdl-build/<arch>/lib/libSDL2.a  libSDL2_ttf.a   (PIC, linkable into a .so)
#   tizen/hello/sdl-build/<arch>/link-libs.txt   the system libraries the archives need (-lm ...)
#   tizen/hello/sdl-build/<arch>/pins.txt        the versions, for the library's log
# Tarballs are cached in tizen/hello/sdl-build/dl; the build trees in sdl-build/work are removed
# after a successful build.
#
# What is built, and why it is not the repo's usual source:
#  * The application itself links the television's SHARED SDL2 and the NDK's SDL2_ttf (2.24.1 / 2.0.18
#    in the NDK sysroot) and builds neither, so there is no repo pin to reuse. Tizen ships no SDL at
#    all. These are the upstream releases below, sha256-pinned (SDL2's equals GitHub's own release-asset
#    digest; SDL2_ttf publishes none, so its value is the digest of the bytes first downloaded).
#  * SDL2_ttf uses its own bundled FreeType (the one upstream tests); HarfBuzz is off (Cyrillic and
#    Latin need no shaping, and this keeps the probe small). The app's FreeType/HarfBuzz pins in
#    ci/dependencies.json belong to libass; swapping them in is `--disable-freetype-builtin` plus a
#    FreeType prefix, left for when a real port needs it.
#  * SDL2 is built with only what the probe needs: the video subsystem's SURFACE half (SDL_Surface and
#    blits live there) with the dummy driver, timers, events, threads, file, loadso, cpuinfo. No audio,
#    joystick, haptic, sensor, render, GL/GLES/Vulkan, Wayland/X11/KMS, udev, dbus, IME. The window and
#    EGL stay ours (Ecore_Wl2); SDL never opens a window.
#
# Toolchains (the same ones build.sh uses for the library):
#   arm     the webOS NDK gcc (WEBOS_SDK; default ~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot) with
#           its glibc-2.12 sysroot, -march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=softfp (no NEON: the
#           Rust side targets plain ARMv7 too) and -tno-glibc-polyfill, as the library link does.
#   x86_64  Linux only: the host gcc (X86_SYSROOT, optional, as in build.sh).
#   i686    Linux only: i686-linux-gnu-gcc, else `gcc -m32` (needs gcc-multilib).
#   host    the machine's own cc, for the unit test on macOS or Linux.
# Needs: bash, curl, tar, make, a C compiler, sha256sum or shasum. No cmake, no pkg-config, no python.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"      # tizen/hello
SDLDIR="$HERE/sdl"
CACHE="$HERE/sdl-build"

SDL_VER=2.32.10
SDL_FILE="SDL2-$SDL_VER.tar.gz"
SDL_SHA=5f5993c530f084535c65a6879e9b26ad441169b3e25d789d83287040a9ca5165
SDL_URLS=("https://github.com/libsdl-org/SDL/releases/download/release-$SDL_VER/$SDL_FILE"
          "https://www.libsdl.org/release/$SDL_FILE")
TTF_VER=2.24.0
TTF_FILE="SDL2_ttf-$TTF_VER.tar.gz"
TTF_SHA=0b2bf1e7b6568adbdbc9bb924643f79d9dedafe061fa1ed687d1d9ac4e453bfd
TTF_URLS=("https://github.com/libsdl-org/SDL_ttf/releases/download/release-$TTF_VER/$TTF_FILE"
          "https://www.libsdl.org/projects/SDL_ttf/release/$TTF_FILE")

ARCH="${1:-}"
FORCE=0
[ "${2:-}" != --force ] || FORCE=1
case "$ARCH" in arm|x86_64|i686|host) ;; *) echo "usage: $0 arm|x86_64|i686|host [--force]" >&2; exit 2 ;; esac

say() { printf '\n== sdl[%s] %s\n' "$ARCH" "$*"; }
die() { echo "build-sdl.sh: $*" >&2; exit 1; }

# autoconf and libtool break on a path with a space (WSL checkouts often sit under /mnt/c/Users/...).
case "$HERE" in *[[:space:]]*) die "the checkout path contains a space ($HERE); autoconf cannot build there. Clone to a path without spaces (e.g. ~/plx)." ;; esac
for t in curl tar make; do command -v "$t" >/dev/null 2>&1 || die "$t not found (apt install build-essential curl)"; done

sha256_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
jobs() { nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2; }

# --- the toolchain of this arch -------------------------------------------------------------
CC_CMD=""; HOST_ARGS=(); ARCH_CFLAGS=""; LD_EXTRA=""; BIN=""; PREFIX_AR=""
EXTRA_SDL=(); EXTRA_PATH=""
case "$ARCH" in
  arm)
    sdk="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
    sysroot="$sdk/arm-webos-linux-gnueabi/sysroot"
    [ -x "$sdk/bin/arm-webos-linux-gnueabi-gcc" ] || die "webOS NDK gcc not found at $sdk/bin (set WEBOS_SDK)"
    [ -d "$sysroot" ] || die "NDK sysroot missing: $sysroot"
    # Absolute path, not a PATH lookup: the Buildroot toolchain wrapper dies with "readlink: No such
    # file or directory" when found through PATH. It does not find its own sysroot either: pass it.
    CC_CMD="$sdk/bin/arm-webos-linux-gnueabi-gcc --sysroot=$sysroot"
    HOST_ARGS=(--host=arm-webos-linux-gnueabi)
    ARCH_CFLAGS="-march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=softfp"
    # configure's link tests must see the same libc the library links: no polyfill shim (see build.sh).
    LD_EXTRA="-tno-glibc-polyfill"
    EXTRA_PATH="$sdk/bin"
    PREFIX_AR="$sdk/bin/arm-webos-linux-gnueabi-"
    ;;
  x86_64|i686|host)
    if [ "$ARCH" != host ]; then
      [ "$(uname -s)" = Linux ] || die "--arch $ARCH builds on Linux (the tester's WSL2), not $(uname -s); use 'host' for the unit test"
    fi
    if [ "$ARCH" = i686 ]; then
      if command -v i686-linux-gnu-gcc >/dev/null 2>&1; then CC_CMD="i686-linux-gnu-gcc"
      else command -v gcc >/dev/null 2>&1 || die "need a 32-bit gcc: apt install gcc-i686-linux-gnu (or gcc-multilib)"; CC_CMD="gcc -m32"; fi
      HOST_ARGS=(--host=i686-linux-gnu)
    else
      command -v "${CC:-cc}" >/dev/null 2>&1 || die "no C compiler: apt install build-essential"
      CC_CMD="${CC:-cc}"
    fi
    [ -z "${X86_SYSROOT:-}" ] || [ "$ARCH" = host ] || CC_CMD="$CC_CMD --sysroot=$X86_SYSROOT"
    if [ "$(uname -s)" = Darwin ]; then
      # No Cocoa/Metal windowing: SDL_Surface and the dummy driver are all the probe uses.
      EXTRA_SDL=(--disable-video-cocoa --disable-video-metal --disable-render-metal --disable-joystick-mfi)
    fi
    ;;
esac
[ -z "$EXTRA_PATH" ] || export PATH="$EXTRA_PATH:$PATH"
AR="${PREFIX_AR}ar"; RANLIB="${PREFIX_AR}ranlib"
NM="${PREFIX_AR}nm"
command -v "$AR" >/dev/null 2>&1 || [ -x "$AR" ] || die "no ar ($AR)"
CFLAGS_ALL="-O2 -g0 -fPIC -ffunction-sections -fdata-sections $ARCH_CFLAGS"

# Everything SDL2 is built WITHOUT. Explicit, not left to autodetection: a Linux box that happens to
# have X11/Wayland/udev/dbus/ALSA dev packages must not grow libraries or dlopen paths the TV lacks.
SDL_CONFIG_FLAGS=(
  --disable-shared --enable-static --with-pic
  --disable-audio --disable-joystick --disable-haptic --disable-hidapi --disable-sensor
  --disable-power --disable-render
  --disable-video-opengl --disable-video-opengles --disable-video-opengles1 --disable-video-opengles2
  --disable-video-vulkan --disable-video-wayland --disable-video-x11 --disable-video-kmsdrm
  --disable-video-rpi --disable-video-vivante --disable-video-offscreen --disable-video-directfb
  --disable-libudev --disable-dbus --disable-ime --disable-ibus --disable-fcitx
  --disable-pipewire --disable-pulseaudio --disable-alsa --disable-jack --disable-sndio
  --disable-esd --disable-arts --disable-nas --disable-libsamplerate
  ${EXTRA_SDL[@]+"${EXTRA_SDL[@]}"}
)
TTF_CONFIG_FLAGS=(--disable-shared --enable-static --with-pic --disable-sdltest --disable-harfbuzz --enable-freetype-builtin)

OUT="$CACHE/$ARCH"
STAMP_WANT="$( { echo "$SDL_VER $SDL_SHA $TTF_VER $TTF_SHA"; echo "$ARCH|$CC_CMD|$CFLAGS_ALL|$LD_EXTRA"; echo "${SDL_CONFIG_FLAGS[*]}"; echo "${TTF_CONFIG_FLAGS[*]}"
                cat "$0" "$SDLDIR/layout-check.c"; } | { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; } | cut -d' ' -f1)"
if [ "$FORCE" = 0 ] && [ -f "$OUT/STAMP" ] && [ "$(cat "$OUT/STAMP")" = "$STAMP_WANT" ] \
   && [ -f "$OUT/lib/libSDL2.a" ] && [ -f "$OUT/lib/libSDL2_ttf.a" ] && [ -f "$OUT/link-libs.txt" ]; then
  echo "sdl[$ARCH]: up to date ($OUT; SDL2 $SDL_VER, SDL2_ttf $TTF_VER). --force rebuilds."
  exit 0
fi

# --- the pinned sources -----------------------------------------------------------------------
fetch() { # file sha url...
  local file="$1" sha="$2"; shift 2
  mkdir -p "$CACHE/dl"
  if [ -f "$CACHE/dl/$file" ] && [ "$(sha256_of "$CACHE/dl/$file")" = "$sha" ]; then return 0; fi
  rm -f "$CACHE/dl/$file"
  local url tmp
  for url in "$@"; do
    tmp="$CACHE/dl/$file.part.$$"
    echo "fetching $url"
    if curl -fsSL --connect-timeout 15 --retry 2 --retry-delay 2 --max-time 300 -o "$tmp" "$url"; then
      if [ "$(sha256_of "$tmp")" = "$sha" ]; then mv "$tmp" "$CACHE/dl/$file"; return 0; fi
      rm -f "$tmp"
      die "$url served bytes whose sha256 is not the pinned $sha; refusing to build from them"
    fi
    rm -f "$tmp"
  done
  die "could not download $file from any source (network? the pin is $sha)"
}
say "sources (SDL2 $SDL_VER, SDL2_ttf $TTF_VER)"
fetch "$SDL_FILE" "$SDL_SHA" "${SDL_URLS[@]}"
fetch "$TTF_FILE" "$TTF_SHA" "${TTF_URLS[@]}"

WORK="$CACHE/work/$ARCH"
rm -rf "$WORK" "$OUT"
mkdir -p "$WORK" "$OUT"
tar -xzf "$CACHE/dl/$SDL_FILE" -C "$WORK"
tar -xzf "$CACHE/dl/$TTF_FILE" -C "$WORK"
SDL_SRC="$WORK/SDL2-$SDL_VER"; TTF_SRC="$WORK/SDL2_ttf-$TTF_VER"
[ -x "$SDL_SRC/configure" ] && [ -x "$TTF_SRC/configure" ] || die "an archive has no ./configure (unexpected layout)"

# ./configure treats the source dir as read-only and builds out of tree.
run_configure() { # srcdir builddir prefix flags... ; the compiler and flags go in the environment
  local src="$1" bld="$2" prefix="$3"; shift 3
  mkdir -p "$bld"
  ( cd "$bld" && "$src/configure" ${HOST_ARGS[@]+"${HOST_ARGS[@]}"} --prefix="$prefix" "$@" \
      CC="$CC_CMD" CFLAGS="$CFLAGS_ALL" LDFLAGS="$LD_EXTRA" ) > "$bld/configure.log" 2>&1 \
    || { tail -40 "$bld/configure.log" >&2; die "configure failed in $bld (full log: $bld/configure.log; the work tree is kept)"; }
}

say "SDL2 $SDL_VER: configure, make"
run_configure "$SDL_SRC" "$WORK/sdl" "$OUT" "${SDL_CONFIG_FLAGS[@]}"
( cd "$WORK/sdl" && make -j"$(jobs)" >make.log 2>&1 && make install >>make.log 2>&1 ) \
  || { tail -40 "$WORK/sdl/make.log" >&2; die "SDL2 build failed (log: $WORK/sdl/make.log)"; }

say "SDL2_ttf $TTF_VER (bundled FreeType, no HarfBuzz): configure, make"
run_configure "$TTF_SRC" "$WORK/ttf" "$OUT" --with-sdl-prefix="$OUT" "${TTF_CONFIG_FLAGS[@]}"
( cd "$WORK/ttf" && make -j"$(jobs)" libSDL2_ttf.la >make.log 2>&1 ) \
  || { tail -40 "$WORK/ttf/make.log" >&2; die "SDL2_ttf build failed (log: $WORK/ttf/make.log)"; }
# libtool's own libSDL2_ttf.a has libSDL2.a nested inside it as a member, which no linker unpacks.
# The library is SDL_ttf.c plus the FreeType modules: archive exactly those objects.
objs="$(cd "$WORK/ttf" && find . -name 'libSDL2_ttf_la-*.o' | sort)"
[ "$(wc -l <<<"$objs")" -ge 40 ] || die "expected SDL_ttf.o plus ~42 FreeType objects, found: $(wc -l <<<"$objs")"
( cd "$WORK/ttf" && rm -f "$OUT/lib/libSDL2_ttf.a" && "$AR" rcs "$OUT/lib/libSDL2_ttf.a" $objs )
cp "$TTF_SRC/SDL_ttf.h" "$OUT/include/SDL2/SDL_ttf.h"

# What the archives need from the system, for build.rs: sdl2-config's static line minus the -L, the
# archive path and -lSDL2 itself (build.rs links the archives by absolute path, never by -l name: the
# NDK sysroot's own libSDL2.a sits on the library path and a -l lookup could pick it).
"$OUT/bin/sdl2-config" --static-libs | tr ' ' '\n' | awk 'NF && $0 !~ /^-L/ && $0 !~ /\.a$/ && $0 != "-lSDL2"' > "$OUT/link-libs.txt"
echo "SDL2 $SDL_VER, SDL2_ttf $TTF_VER (bundled FreeType, no HarfBuzz)" > "$OUT/pins.txt"

# --- proofs -----------------------------------------------------------------------------------
say "checks"
# The Rust side hard-codes a few struct offsets and constants; compile (never link) a file that
# asserts them against the headers THIS build installed, with this arch's compiler.
# shellcheck disable=SC2086
$CC_CMD $CFLAGS_ALL -I"$OUT/include/SDL2" -std=c11 -c "$SDLDIR/layout-check.c" -o /dev/null \
  || die "layout-check.c failed: sdl/layout-check.c and plx/src/sdl.rs no longer match the SDL headers"
echo "layout-check.c: struct offsets and constants match the installed headers"
syms="$("$NM" -A "$OUT/lib/libSDL2.a" "$OUT/lib/libSDL2_ttf.a" 2>/dev/null || true)"
for s in SDL_Init SDL_CreateRGBSurfaceWithFormat SDL_UpperBlit SDL_ConvertSurfaceFormat TTF_Init TTF_OpenFont TTF_RenderUTF8_Blended FT_Init_FreeType; do
  grep -Eq " T _?$s\$" <<<"$syms" || die "archives define no $s"
done
echo "archives define the SDL/TTF/FreeType entry points the probe calls"
echo "link-libs: $(tr '\n' ' ' < "$OUT/link-libs.txt")"
printf '%s\n' "$STAMP_WANT" > "$OUT/STAMP"
rm -rf "$WORK"
ls -l "$OUT/lib/libSDL2.a" "$OUT/lib/libSDL2_ttf.a"
echo "sdl[$ARCH]: done -> $OUT"
