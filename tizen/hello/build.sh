#!/usr/bin/env bash
# Build the step-1 Samsung Tizen "hello world" (+ the H5 static-SDL probe and its on-screen diagnostics
# panel): libplx.so (Rust, with SDL2 and SDL2_ttf linked in statically) and the .NET .tpk that carries it.
# The panel header shows the git short sha this script embeds (PLX_GIT_SHA; "+dirty" when tizen/hello
# has uncommitted changes): build a tester's package from a clean, pushed commit.
#
#   tizen/hello/build.sh [--arch arm|x86_64|i686] [--so-only | --tpk-only]
#                        [--log-host IPV4:PORT] [--mock-host HOST:PORT] [--no-gfx]
#                        [--author-p12 FILE] [--dist-p12 FILE]
#   tizen/hello/build.sh --host-test       # the H5 unit test on this machine (macOS or Linux), no TV
#
#   --arch arm      (default) real TVs, 32-bit ARMv7 softfp. Built on this Mac with the webOS NDK's
#                   gcc and glibc-2.12 sysroot (the same NDK the app uses).
#   --arch x86_64   the Tizen TV emulator, TV Extension 10.0 (its only image is x86_64). Built on a
#                   LINUX box (the tester's WSL2) with plain host cargo and the host gcc.
#   --arch i686     the older 32-bit 9.0 emulator image. Linux only; needs a 32-bit gcc:
#                   `apt install gcc-i686-linux-gnu` (or gcc-multilib).
#   --so-only       build libplx.so and verify it; skip the .NET step (WSL without dotnet).
#   --tpk-only      package an existing out/<arch>/libplx.so (build the .so elsewhere first).
#   --host-test     build the static SDL2 + SDL2_ttf for THIS machine (sdl/build-sdl.sh host), then run
#                   `cargo test` with them linked, so the H5 test renders the label's text AND the diagnostics
#                   panel to an SDL_Surface from the shipped font (no GL, no window, no TV). Plain `cargo test`
#                   in plx/ also works but skips those tests (no SDL linked). Takes no other flag.
#   --log-host IPV4:PORT   OPTIONAL. Ship the address in the package (res/log-host.txt); the app then sends
#                   every log line, host and library, as a UDP datagram to it. From the emulator the build
#                   machine is usually 10.0.2.2. Receive with `nc -klu PORT`. Without the flag the package
#                   carries no address and nothing is sent: the on-screen panel is always on and is what a
#                   tester with a retail TV photographs (README, "Tester build").
#   --mock-host HOST:PORT  OPTIONAL. Ship the address of a mock Plex server (tests/mock_pms.py) in the package
#                   (res/mock-host.txt, written exactly as res/log-host.txt is); step 9 "net" of the panel then
#                   makes `GET http://HOST:PORT/identity` through the app's own libcurl binding and shows the
#                   HTTP status and the machineIdentifier. HOST is an IPv4 literal or a DNS name. Without the
#                   flag step 9 only reports whether libcurl loaded. README, "H8".
#   --no-gfx        Build WITHOUT the application's renderer (the `gfx` cargo feature of plx/): step 8 "gfx" of
#                   the panel then says so. The default links plx_gfx in; use this when that link fails (it is
#                   the fallback if the x86_64 link in WSL cannot be made, README "H7") or to measure the
#                   library without it.
#   --author-p12 FILE, --dist-p12 FILE   the signing certificates (see "Signing" below).
#
# Signing: building the .tpk REQUIRES a Samsung certificate profile (author + VD Public distributor
# certificate, bound to the target device's DUID) and refuses to start without it. The SDK's bundled
# default certificates are expired and no TV trusts them, so there is NO fallback to them. Required:
#   PLX_TIZEN_AUTHOR_P12 (or --author-p12)   the author certificate, a .p12
#   PLX_TIZEN_AUTHOR_PASS                    its password
#   PLX_TIZEN_DIST_P12 (or --dist-p12)       the distributor certificate, a .p12
#   PLX_TIZEN_DIST_PASS                      its password
# Passwords are environment-only (a flag would land in shell history and `ps`), are handed to MSBuild
# as the AuthorPass/DistributorPass environment properties (never on its command line) and are never
# printed. No certificate or password is ever committed (*.p12 and *.pfx are gitignored). --so-only
# needs none of this. README: "Signing".
#
# H5 (static SDL2 + SDL2_ttf): the library statically links SDL2 2.32.10 and SDL2_ttf 2.24.0, built from
# the sha256-pinned upstream tarballs by sdl/build-sdl.sh (run for you, once per arch, cached in
# tizen/hello/sdl-build/; needs curl, make and the arch's C compiler, nothing else). The package also
# carries res/appfont.ttf (a copy of the repository's pkg/appfont.ttf, Inter, SIL OFL) and res/OFL.txt.
# Verified after the build: no DT_NEEDED outside the C runtime (no libSDL2), the SDL/TTF code present
# inside the object but not exported.
#
# H7 (the application's renderer, plx_gfx, `--features gfx`, the default): plx_gfx binds GLES2 at LINK time,
# which a library that must not name libGLESv2 in DT_NEEDED cannot do, so plx/src/gl_shim.rs defines every gl*
# function plx_gfx imports and forwards it to the libGLESv2 the library dlopens. Verified after the build: no
# undefined gl* symbol remains (the existing "only getauxval" check), the forwarders are exported, plx_gfx's
# code is inside the object (or, with --no-gfx, is not).
#
# Output (all gitignored):
#   tizen/hello/out/<arch>/libplx.so       the library
#   tizen/hello/out/PlxHello-<arch>.tpk    the package, signed with the certificates above
#   tizen/hello/sdl-build/<arch>/          the static SDL2/SDL2_ttf archives and headers (a cache)
#
# Environment (all optional unless the .tpk is built; see Signing):
#   RUST_TOOLCHAIN       cargo toolchain. Default: arm = the repo's pin (rust-modules/rust-toolchain.toml,
#                        else 1.100.0); x86_64/i686 = whatever `cargo` is by default.
#   WEBOS_SDK            the webOS NDK root (default ~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot;
#                        the Makefile's own default; `make print-WEBOS_SDK` does not exist).
#   DOTNET_ROOT          user-local .NET 8 SDK (default ~/.dotnet; install with
#                        `dotnet-install.sh --channel 8.0 --install-dir ~/.dotnet`). Nothing is
#                        installed system-wide.
#   PLX_MAX_GLIBC        highest GLIBC_x.y symbol version allowed; the build FAILS above it. Default 2.30
#                        (arm, i686: the oldest glibc nuvio's Tizen notes mention) and 2.39 for x86_64
#                        (the Tizen 10 emulator image). Both numbers are from those notes, not measured.
#   PLX_ALLOW_NEW_GLIBC  1 = report instead of fail.
#   X86_SYSROOT          for x86_64/i686 on a host whose glibc is newer than the TV's: a sysroot with an
#                        older glibc (Debian buster, or a Tizen rootstrap). Passed as --sysroot (also to the
#                        SDL build). UNTESTED.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PLX="$HERE/plx"
HOST="$HERE/host/PlxHello"
OUT="$HERE/out"
REPO="$(cd "$HERE/../.." && pwd)"

ARCH=arm
DO_SO=1
DO_TPK=1
HOST_TEST=0
LOG_HOST=""
MOCK_HOST=""
NO_GFX=0
AUTHOR_P12="${PLX_TIZEN_AUTHOR_P12:-}"
DIST_P12="${PLX_TIZEN_DIST_P12:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="${2:?--arch needs arm, x86_64 or i686}"; shift 2 ;;
    --arch=*) ARCH="${1#--arch=}"; shift ;;
    --so-only) DO_TPK=0; shift ;;
    --tpk-only) DO_SO=0; shift ;;
    --host-test) HOST_TEST=1; shift ;;
    --log-host) LOG_HOST="${2:?--log-host needs IPV4:PORT}"; shift 2 ;;
    --log-host=*) LOG_HOST="${1#--log-host=}"; shift ;;
    --mock-host) MOCK_HOST="${2:?--mock-host needs HOST:PORT}"; shift 2 ;;
    --mock-host=*) MOCK_HOST="${1#--mock-host=}"; shift ;;
    --no-gfx) NO_GFX=1; shift ;;
    --author-p12) AUTHOR_P12="${2:?--author-p12 needs a file}"; shift 2 ;;
    --author-p12=*) AUTHOR_P12="${1#--author-p12=}"; shift ;;
    --dist-p12) DIST_P12="${2:?--dist-p12 needs a file}"; shift 2 ;;
    --dist-p12=*) DIST_P12="${1#--dist-p12=}"; shift ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d;s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
done

case "$ARCH" in
  arm)    TRIPLE=armv7-unknown-linux-gnueabi ;;
  x86_64) TRIPLE=x86_64-unknown-linux-gnu ;;
  i686)   TRIPLE=i686-unknown-linux-gnu ;;
  *) echo "--arch must be arm, x86_64 or i686 (got $ARCH)" >&2; exit 2 ;;
esac
TRIPLE_ENV="$(echo "$TRIPLE" | tr 'a-z-' 'A-Z_')"   # for CARGO_TARGET_<TRIPLE>_LINKER
SO="$OUT/$ARCH/libplx.so"
TPK="$OUT/PlxHello-$ARCH.tpk"

say() { printf '\n== %s\n' "$*"; }
die() { echo "build.sh: $*" >&2; exit 1; }

# --- --host-test: the H5 unit test on this machine, with the static SDL linked --------------------
if [ "$HOST_TEST" = 1 ]; then
  [ "$DO_SO" = 1 ] && [ "$DO_TPK" = 1 ] && [ -z "$LOG_HOST" ] && [ -z "$MOCK_HOST" ] || die "--host-test takes no other flag (except --no-gfx)"
  export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PLX/target"
  say "static SDL2 + SDL2_ttf for this machine (sdl/build-sdl.sh host; cached)"
  "$HERE/sdl/build-sdl.sh" host
  say "cargo test (PLX_SDL_PREFIX=$HERE/sdl-build/host): the H5 label test renders to an SDL_Surface, no GL"
  # shellcheck disable=SC2086
  PLX_SDL_PREFIX="$HERE/sdl-build/host" cargo ${RUST_TOOLCHAIN:+"+$RUST_TOOLCHAIN"} test --manifest-path "$PLX/Cargo.toml" 2>&1 \
    | grep -E '^(test |running )|test result|panicked|FAILED|^(error|warning)' || true
  [ "${PIPESTATUS[0]}" = 0 ] || die "cargo test failed"
  if [ "$NO_GFX" = 0 ]; then
    # The same suite with plx_gfx linked in: the gl_shim tests (the forwarders, and the drift test that fails
    # when plx_gfx imports a gl* function the shim lacks) and the shader table's checks against gfx.rs run only
    # here. Needs the host's SDL2 headers/libs for plx_gfx's own link (gfx/build.rs); --no-gfx skips it.
    say "cargo test --features gfx (plx_gfx linked: the GL forwarders and the shader table's drift tests)"
    # shellcheck disable=SC2086
    PLX_SDL_PREFIX="$HERE/sdl-build/host" cargo ${RUST_TOOLCHAIN:+"+$RUST_TOOLCHAIN"} test --manifest-path "$PLX/Cargo.toml" --features gfx 2>&1 \
      | grep -E '^(test |running )|test result|panicked|FAILED|^(error|warning)' || true
    [ "${PIPESTATUS[0]}" = 0 ] || die "cargo test --features gfx failed (a host without plx_gfx's link dependencies can pass --no-gfx)"
  fi
  echo "(PLX_H5_DUMP=/path/label.bmp and PLX_PANEL_DUMP=/path/stem write the rendered label and panels as BMPs; add -- --nocapture to read the log lines)"
  exit 0
fi

# --- the log host and the signing material (checked before any long build) ------------------
if [ -n "$LOG_HOST" ]; then
  [ "$DO_TPK" = 1 ] || die "--log-host goes into the .tpk; it has no meaning with --so-only"
  # IPv4 literal only: the library does no name resolution (a blocking lookup has no place in a logger).
  grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}:[0-9]{1,5}$' <<<"$LOG_HOST" || die "--log-host must be IPV4:PORT, e.g. 10.0.2.2:9999 (got: $LOG_HOST)"
  _lh_port="${LOG_HOST##*:}"
  { [ "$((10#$_lh_port))" -ge 1 ] && [ "$((10#$_lh_port))" -le 65535 ]; } || die "--log-host port out of range: $_lh_port"
  for _lh_octet in $(tr '.:' '  ' <<<"${LOG_HOST%:*}"); do
    [ "$((10#$_lh_octet))" -le 255 ] || die "--log-host address octet out of range: $_lh_octet"
  done
fi

# --mock-host: HOST:PORT where HOST is an IPv4 literal (four octets, each 0-255) or a DNS name, never a URL
# (no scheme, no path: the probe builds http://HOST:PORT/identity itself). The same rules the library applies
# when it reads res/mock-host.txt (plx/src/net_probe.rs parse_mock_host), so a value this accepts is one the
# library accepts.
if [ -n "$MOCK_HOST" ]; then
  [ "$DO_TPK" = 1 ] || die "--mock-host goes into the .tpk; it has no meaning with --so-only"
  grep -Eq '^[A-Za-z0-9.-]+:[0-9]{1,5}$' <<<"$MOCK_HOST" || die "--mock-host must be HOST:PORT, e.g. 10.0.2.2:32499 (no http://, no path; got: $MOCK_HOST)"
  _mh_host="${MOCK_HOST%:*}"; _mh_port="${MOCK_HOST##*:}"
  { [ "$((10#$_mh_port))" -ge 1 ] && [ "$((10#$_mh_port))" -le 65535 ]; } || die "--mock-host port out of range: $_mh_port"
  if grep -Eq '^[0-9.]+$' <<<"$_mh_host"; then
    grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' <<<"$_mh_host" || die "--mock-host $_mh_host is not an IPv4 address (four numbers 0-255)"
    for _mh_octet in $(tr '.' ' ' <<<"$_mh_host"); do
      [ "$((10#$_mh_octet))" -le 255 ] || die "--mock-host address octet out of range: $_mh_octet"
    done
  else
    grep -Eq '^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*$' <<<"$_mh_host" || die "--mock-host $_mh_host is not a host name (letters, digits, '-' and '.', no empty label)"
  fi
fi

# Refuse to build the .tpk without a Samsung certificate profile. Names the missing variables, never
# prints a password, never falls back to the SDK's bundled (expired) default certificates.
check_signing() {
  local missing=""
  [ -n "$AUTHOR_P12" ] || missing="$missing PLX_TIZEN_AUTHOR_P12(or --author-p12)"
  [ -n "${PLX_TIZEN_AUTHOR_PASS:-}" ] || missing="$missing PLX_TIZEN_AUTHOR_PASS"
  [ -n "$DIST_P12" ] || missing="$missing PLX_TIZEN_DIST_P12(or --dist-p12)"
  [ -n "${PLX_TIZEN_DIST_PASS:-}" ] || missing="$missing PLX_TIZEN_DIST_PASS"
  if [ -n "$missing" ]; then
    die "refusing to build the .tpk: no signing material. Missing:$missing
  A TV or emulator installs only a package signed with a Samsung certificate profile (author + VD Public
  distributor certificate, bound to the device's DUID); the SDK's bundled default certificates are expired
  and are never used. Create the profile (README, \"Signing\"), then:
    PLX_TIZEN_AUTHOR_P12=/path/author.p12 PLX_TIZEN_AUTHOR_PASS=... \\
    PLX_TIZEN_DIST_P12=/path/distributor.p12 PLX_TIZEN_DIST_PASS=... tizen/hello/build.sh ...
  (or --so-only, which needs no certificates)"
  fi
  [ -f "$AUTHOR_P12" ] || die "author certificate not found: $AUTHOR_P12"
  [ -f "$DIST_P12" ] || die "distributor certificate not found: $DIST_P12"
  # MSBuild runs in the host project's directory: hand it absolute paths.
  AUTHOR_P12="$(cd "$(dirname "$AUTHOR_P12")" && pwd)/$(basename "$AUTHOR_P12")"
  DIST_P12="$(cd "$(dirname "$DIST_P12")" && pwd)/$(basename "$DIST_P12")"
}
[ "$DO_TPK" = 0 ] || check_signing

# MSBuild's own escaping (its special characters as %XX), '%' first. A shell function, so the value
# never appears in a process's argument list.
msb_escape() {
  local v="$1"
  v="${v//%/%25}"; v="${v//\*/%2A}"; v="${v//\?/%3F}"; v="${v//@/%40}"; v="${v//\$/%24}"
  v="${v//"("/%28}"; v="${v//")"/%29}"; v="${v//;/%3B}"; v="${v//\'/%27}"
  printf '%s' "$v"
}

# --- the Rust library ---------------------------------------------------------------------
build_so() {
  export PATH="$HOME/.cargo/bin:$PATH"
  # Per the repo's rules for hand-run cargo: no incremental cache, target dir inside the tree
  # (gitignored), never /tmp.
  export CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PLX/target"

  # The revision the panel header shows, embedded by build.rs. "+dirty" when anything under tizen/hello
  # (untracked files included, gitignored build output not) differs from HEAD, so a photographed screen
  # says whether it came from a committed tree. PLX_GIT_SHA in the environment wins.
  if [ -z "${PLX_GIT_SHA:-}" ]; then
    local sha; sha="$(git -C "$HERE" rev-parse --short=9 HEAD 2>/dev/null || true)"
    if [ -z "$sha" ]; then
      sha=unknown
    elif [ -n "$(git -C "$HERE" status --porcelain -- "$HERE" 2>/dev/null)" ]; then
      sha="$sha+dirty"
    fi
    export PLX_GIT_SHA="$sha"
  fi
  say "panel revision: $PLX_GIT_SHA"

  local toolchain="${RUST_TOOLCHAIN:-}"
  local flags="-C link-arg=-Wl,-soname,libplx.so"
  local bin_prefix=""   # binutils prefix for the verification step

  if [ "$ARCH" = arm ]; then
    [ -n "$toolchain" ] || toolchain="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "$HERE/../../rust-modules/rust-toolchain.toml" 2>/dev/null || true)"
    [ -n "$toolchain" ] || toolchain=1.100.0
    local sdk="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
    local sysroot="$sdk/arm-webos-linux-gnueabi/sysroot"
    bin_prefix="$sdk/bin/arm-webos-linux-gnueabi-"
    [ -x "${bin_prefix}gcc" ] || die "webOS NDK gcc not found at ${bin_prefix}gcc (set WEBOS_SDK; the repo's setup-env make target installs the NDK)"
    [ -d "$sysroot" ] || die "NDK sysroot missing: $sysroot"
    export "CARGO_TARGET_${TRIPLE_ENV}_LINKER=${bin_prefix}gcc"
    # -tno-glibc-polyfill: the NDK gcc otherwise links its webOS `libglibc_polyfills` shim, which
    # would paper over the gap between the 2.12 sysroot and the television's glibc. Tizen has a
    # real, newer glibc; nothing is polyfilled.
    flags="$flags -C link-arg=--sysroot=$sysroot -C link-arg=-tno-glibc-polyfill -C link-arg=-Wl,-rpath-link,$sysroot/usr/lib -L native=$sysroot/usr/lib"
  else
    [ "$(uname -s)" = Linux ] || die "--arch $ARCH builds on Linux (the tester's WSL2), not $(uname -s); see README"
    if [ "$ARCH" = i686 ]; then
      if command -v i686-linux-gnu-gcc >/dev/null 2>&1; then
        export "CARGO_TARGET_${TRIPLE_ENV}_LINKER=i686-linux-gnu-gcc"
      else
        command -v gcc >/dev/null 2>&1 || die "need a 32-bit gcc: apt install gcc-i686-linux-gnu (or gcc-multilib)"
        export "CARGO_TARGET_${TRIPLE_ENV}_LINKER=gcc"
        flags="$flags -C link-arg=-m32"
      fi
    fi
    [ -z "${X86_SYSROOT:-}" ] || flags="$flags -C link-arg=--sysroot=$X86_SYSROOT"
  fi
  export RUSTFLAGS="$flags"

  # H5: the static SDL2 + SDL2_ttf for this arch (cached; its own toolchain setup mirrors the above).
  # build.rs links them from PLX_SDL_PREFIX by absolute path; set it unconditionally so a .so can never
  # silently ship without the probe.
  say "static SDL2 + SDL2_ttf for $ARCH (sdl/build-sdl.sh; cached in sdl-build/)"
  if [ "$ARCH" = arm ]; then export WEBOS_SDK="$sdk"; fi
  "$HERE/sdl/build-sdl.sh" "$ARCH"
  export PLX_SDL_PREFIX="$HERE/sdl-build/$ARCH"

  local plus=""; [ -z "$toolchain" ] || plus="+$toolchain"
  # shellcheck disable=SC2086
  if ! rustup $plus target list --installed 2>/dev/null | grep -qx "$TRIPLE"; then
    say "rustup target add $TRIPLE ${toolchain:+(toolchain $toolchain)}"
    rustup target add ${toolchain:+--toolchain "$toolchain"} "$TRIPLE"
  fi

  # H7: the application's renderer. On a non-ARM target plx_gfx's own build script emits `-lSDL2 -lSDL2_ttf
  # -lGL` (it is written for the desktop simulator), and those would put libSDL2 / libGL in DT_NEEDED, which
  # verify_so refuses. Put the prefix's STATIC archives first on the search path so `-lSDL2` / `-lSDL2_ttf`
  # find them (the .a files this script built are the only ones there), and give `-lGL` an empty archive:
  # plx_gfx's GL imports are the library's own forwarders (gl_shim.rs), so nothing is needed from libGL.
  # (Not built or run on the machine this was written on: README "H7", "What is not verified".)
  local features=""
  if [ "$NO_GFX" = 0 ]; then
    features="--features gfx"
    if [ "$ARCH" != arm ]; then
      command -v ar >/dev/null 2>&1 || die "need ar (binutils) for the empty libGL.a the x86 gfx link uses; or build with --no-gfx"
      ( cd "$PLX_SDL_PREFIX/lib" && rm -f libGL.a && ar rcs libGL.a )
      export RUSTFLAGS="$RUSTFLAGS -L native=$PLX_SDL_PREFIX/lib"
    fi
  fi

  say "cargo ${plus} build --release --target $TRIPLE ${features:-(no gfx feature: --no-gfx)}"
  # shellcheck disable=SC2086
  if ! cargo $plus build --release --manifest-path "$PLX/Cargo.toml" --target "$TRIPLE" $features; then
    [ -z "$features" ] || echo "build.sh: the build with plx_gfx failed; if it is the gfx link (see the messages above), build.sh --no-gfx builds the library without it and the panel's step 8 says so" >&2
    die "cargo build failed"
  fi
  mkdir -p "$OUT/$ARCH"
  cp "$PLX/target/$TRIPLE/release/libplx.so" "$SO"

  verify_so "$bin_prefix"
}

verify_so() {
  local p="$1"
  local readelf="${p}readelf" objdump="${p}objdump" nm="${p}nm"
  command -v "$readelf" >/dev/null 2>&1 || { readelf=readelf; objdump=objdump; nm=nm; }
  say "verify $SO"
  file "$SO" || true
  if [ "$ARCH" = arm ]; then
    echo "-- readelf -A (softfp = Tag_ABI_VFP_args ABSENT):"
    "$readelf" -A "$SO"
    local attrs; attrs="$("$readelf" -A "$SO")"
    if grep -q Tag_ABI_VFP_args <<<"$attrs"; then die "library is hard-float (Tag_ABI_VFP_args present); Tizen armel needs softfp"; fi
  fi
  echo "-- DT_NEEDED / SONAME:"
  "$readelf" -d "$SO" | grep -E "NEEDED|SONAME" || true
  # An allowlist, not a deny-list: the C runtime only. A libSDL2 here would mean the static link did not
  # happen; anything else (libudev, libdbus, libX11, libwayland-*, libasound ...) would mean the SDL
  # configure picked a library up from the build machine that the TV may not have.
  local needed bad
  needed="$("$readelf" -d "$SO" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')"
  bad="$(grep -Ev '^(libgcc_s\.so\.1|librt\.so\.1|libpthread\.so\.0|libdl\.so\.2|libc\.so\.6|libm\.so\.6|ld-linux[A-Za-z0-9_.-]*\.so\.[0-9]+)$' <<<"$needed" || true)"
  [ -z "$bad" ] || die "DT_NEEDED outside the C-runtime allowlist: $(echo $bad)"
  if "$readelf" -d "$SO" | grep -q TEXTREL; then die "the library has text relocations (a non-PIC object was linked in)"; fi
  echo "-- size: $(wc -c < "$SO" | tr -d ' ') bytes"
  # The probe's SDL/TTF/FreeType must be INSIDE the object (present as local symbols) and NOT exported or
  # imported. Checked for presence too: "no libSDL2 in DT_NEEDED" also passes on a library with no SDL.
  if [ -n "${PLX_SDL_PREFIX:-}" ]; then
    local all dyn probe
    all="$("$nm" "$SO" 2>/dev/null || true)"
    dyn="$("$nm" -D "$SO" 2>/dev/null || true)"
    for probe in SDL_Init SDL_UpperBlit TTF_Init TTF_RenderUTF8_Blended FT_Init_FreeType; do
      grep -Eq " t $probe\$" <<<"$all" || die "$probe is not a LOCAL symbol of $SO: the static SDL/SDL_ttf is missing or exported"
    done
    if grep -Eq ' (SDL_|TTF_|FT_)[A-Za-z0-9_]+$' <<<"$dyn"; then
      die "SDL/TTF/FreeType names in the dynamic symbol table: $(grep -E ' (SDL_|TTF_|FT_)[A-Za-z0-9_]+$' <<<"$dyn" | head -3 | tr '\n' ' ')"
    fi
    echo "-- static SDL2 + SDL2_ttf + FreeType are inside the object as local symbols; none exported or imported"
  fi
  echo "-- highest versioned glibc symbol:"
  local max; max="$("$objdump" -T "$SO" | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1 || true)"
  echo "   ${max:-none}"
  echo "-- undefined symbols WITHOUT a version and not weak (bound by the device's libc at load time):"
  local unv; unv="$("$readelf" --dyn-syms -W "$SO" | awk '$7=="UND" && $5=="GLOBAL" && $8 !~ /@/ && $8 != "" {print $8}' || true)"
  echo "${unv:-   (none)}" | sed 's/^/   /'
  # Rust's std calls getauxval (glibc 2.16+), absent from the webOS NDK's 2.12 sysroot. Any Tizen
  # set has a glibc >= 2.30, so the unversioned reference binds there. It is the ONLY expected one.
  local unexpected; unexpected="$(grep -vx 'getauxval' <<<"$unv" || true)"
  [ -z "$unexpected" ] || die "unexpected unresolved symbols: $(echo $unexpected)"
  echo "-- exported (every DllImport'd name must appear):"
  "$nm" -D --defined-only "$SO" | sed 's/^/   /'
  # H7: the library either carries the application's renderer, with its GL imports answered by the library's
  # own forwarders (an undefined gl* symbol would have failed the check above: the cdylib is BIND_NOW, so
  # one would stop dlopen of the whole library), or, with --no-gfx, carries none of it.
  local gfx_syms; gfx_syms="$("$nm" "$SO" 2>/dev/null | grep -c 'plx_gfx' || true)"
  if [ "$NO_GFX" = 0 ]; then
    [ "${gfx_syms:-0}" -gt 0 ] || die "$SO holds no plx_gfx code although the gfx feature was requested (stale build?)"
    # Captured first: `grep -q` closing a pipe early would fail the pipeline under pipefail.
    local fw exported; exported="$("$nm" -D --defined-only "$SO" | awk '{print $3}')"
    for fw in glClear glDrawArrays glGetString glBlendFuncSeparate; do
      grep -qx "$fw" <<<"$exported" || die "the GL forwarder $fw is not exported from $SO (gl_shim.rs)"
    done
    echo "-- plx_gfx is inside the object ($gfx_syms symbols); its gl* imports are the library's own forwarders (none undefined)"
  else
    [ "${gfx_syms:-0}" -eq 0 ] || die "$SO holds plx_gfx code although --no-gfx was given"
    echo "-- built without plx_gfx (--no-gfx)"
  fi
  local want got
  want="$(grep -hoE 'plx_tizen_[a-z]+' "$HOST/Program.cs" | sort -u)"
  got="$("$nm" -D --defined-only "$SO" | awk '{print $3}')"
  for f in $want; do grep -qx "$f" <<<"$got" || die "$SO does not export $f"; done
  # The panel header's revision is in the object (a stale cargo cache would leave the old one).
  if [ -n "${PLX_GIT_SHA:-}" ]; then
    LC_ALL=C grep -aFq -- "Samsung probe " "$SO" || die "the panel header text is not in $SO"
    LC_ALL=C grep -aFq -- "$PLX_GIT_SHA" "$SO" || die "the revision $PLX_GIT_SHA is not embedded in $SO (stale build?)"
    echo "-- the panel header embeds revision $PLX_GIT_SHA"
  fi

  local limit="${PLX_MAX_GLIBC:-2.30}"
  [ -n "${PLX_MAX_GLIBC:-}" ] || [ "$ARCH" != x86_64 ] || limit=2.39
  if [ -n "$max" ]; then
    local top; top="$(printf '%s\n%s\n' "GLIBC_$limit" "$max" | sort -V | tail -1)"
    if [ "$top" != "GLIBC_$limit" ]; then
      if [ "${PLX_ALLOW_NEW_GLIBC:-0}" = 1 ]; then
        echo "WARNING: $max is newer than the allowed GLIBC_$limit; a TV with an older glibc will refuse to load it"
      else
        die "$max exceeds GLIBC_$limit: a target with an older glibc refuses to load this. Link against an older sysroot (X86_SYSROOT=..., e.g. Debian buster or a Tizen rootstrap), raise PLX_MAX_GLIBC if you know the target's glibc, or set PLX_ALLOW_NEW_GLIBC=1 to proceed anyway"
      fi
    fi
  fi
}

# --- the .tpk -----------------------------------------------------------------------------
build_tpk() {
  [ -f "$SO" ] || die "$SO is missing; build it first (without --tpk-only)"
  export DOTNET_ROOT="${DOTNET_ROOT:-$HOME/.dotnet}"
  export PATH="$DOTNET_ROOT:$PATH" DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
  command -v dotnet >/dev/null 2>&1 || die "no dotnet; install a user-local SDK: dotnet-install.sh --channel 8.0 --install-dir ~/.dotnet"

  say "stage lib/libplx.so ($ARCH) into the host project"
  rm -rf "$HOST/lib"
  mkdir -p "$HOST/lib"
  cp "$SO" "$HOST/lib/libplx.so"

  # res/ is generated (gitignored): the H5 probe's font with its licence and, with --log-host and
  # --mock-host, log-host.txt and mock-host.txt. A stale res/ or a stale copy in the SDK's staging directory would silently switch the
  # network log on (or leave an old address in), so both go first. The font is the app's own Inter
  # (SIL OFL); the licence travels with it. Copied here, not in build_so, so --tpk-only works on a
  # machine that never built the library.
  rm -rf "$HOST/res" "$HOST/bin/Release/tizen80/tpkroot"
  mkdir -p "$HOST/res"
  [ -f "$REPO/pkg/appfont.ttf" ] && [ -f "$REPO/pkg/OFL.txt" ] || die "pkg/appfont.ttf or pkg/OFL.txt missing from the checkout"
  cp "$REPO/pkg/appfont.ttf" "$HOST/res/appfont.ttf"
  cp "$REPO/pkg/OFL.txt" "$HOST/res/OFL.txt"
  if [ -n "$LOG_HOST" ]; then
    say "log host $LOG_HOST -> res/log-host.txt (the app mirrors its log to it over UDP)"
    printf '%s\n' "$LOG_HOST" > "$HOST/res/log-host.txt"
  fi
  if [ -n "$MOCK_HOST" ]; then
    say "mock host $MOCK_HOST -> res/mock-host.txt (step 9 GETs http://$MOCK_HOST/identity)"
    printf '%s\n' "$MOCK_HOST" > "$HOST/res/mock-host.txt"
  fi

  say "dotnet build -c Release (Tizen.NET.Sdk, tizen80; signed with the supplied certificates)"
  rm -f "$HOST"/bin/Release/tizen80/*.tpk
  # The certificates go in as MSBuild environment properties scoped to this one command (the SDK's
  # TizenPrepareCertificate falls back to its bundled defaults only when AuthorPath/DistributorPath are
  # EMPTY, and check_signing made sure they are not). Passwords stay out of argv, so out of `ps`.
  # Values are MSBuild-escaped (msb_escape): left raw, a "%41" or "@(x)" in a password was decoded or
  # expanded on the way to the Sign task and the p12 then failed with "wrong password". Build servers
  # are off so no long-lived msbuild/compiler process keeps the environment.
  local a_path a_pass d_path d_pass
  a_path="$(msb_escape "$AUTHOR_P12")"; a_pass="$(msb_escape "$PLX_TIZEN_AUTHOR_PASS")"
  d_path="$(msb_escape "$DIST_P12")"; d_pass="$(msb_escape "$PLX_TIZEN_DIST_PASS")"
  (cd "$HOST" && \
    AuthorPath="$a_path" AuthorPass="$a_pass" DistributorPath="$d_path" DistributorPass="$d_pass" \
    dotnet build PlxHello.csproj -c Release -nologo -nodeReuse:false -p:UseSharedCompilation=false)
  unset a_path a_pass d_path d_pass
  local built; built="$(ls "$HOST"/bin/Release/tizen80/*.tpk 2>/dev/null | head -1 || true)"
  [ -n "$built" ] || die "dotnet produced no .tpk"
  mkdir -p "$OUT"
  cp "$built" "$TPK"

  say "verify $TPK"
  local listing; listing="$(unzip -l "$TPK")"
  echo "$listing"
  for need in 'tizen-manifest.xml' 'bin/PlxHello.dll' 'lib/libplx.so' 'res/appfont.ttf' 'res/OFL.txt' 'author-signature.xml' 'signature1.xml'; do
    grep -q " $need\$" <<<"$listing" || die "$TPK lacks $need"
  done
  # The log host is in the package exactly when it was asked for, and is the address that was asked for.
  if [ -n "$LOG_HOST" ]; then
    grep -q ' res/log-host.txt$' <<<"$listing" || die "$TPK lacks res/log-host.txt although --log-host was given"
    [ "$(unzip -p "$TPK" res/log-host.txt | tr -d '\r\n')" = "$LOG_HOST" ] || die "res/log-host.txt in $TPK does not hold $LOG_HOST"
  else
    ! grep -q ' res/log-host.txt$' <<<"$listing" || die "$TPK carries a res/log-host.txt although no --log-host was given (stale staging?)"
  fi
  # Likewise the mock host: in the package exactly when asked for, and the address that was asked for.
  if [ -n "$MOCK_HOST" ]; then
    grep -q ' res/mock-host.txt$' <<<"$listing" || die "$TPK lacks res/mock-host.txt although --mock-host was given"
    [ "$(unzip -p "$TPK" res/mock-host.txt | tr -d '\r\n')" = "$MOCK_HOST" ] || die "res/mock-host.txt in $TPK does not hold $MOCK_HOST"
  else
    ! grep -q ' res/mock-host.txt$' <<<"$listing" || die "$TPK carries a res/mock-host.txt although no --mock-host was given (stale staging?)"
  fi
  # The library in the package must be byte-identical to the one that was verified.
  local tmp; tmp="$(mktemp -d)"
  unzip -qo "$TPK" lib/libplx.so -d "$tmp"
  cmp -s "$tmp/lib/libplx.so" "$SO" || { rm -rf "$tmp"; die "packaged libplx.so differs from $SO"; }
  unzip -qo "$TPK" res/appfont.ttf -d "$tmp"
  cmp -s "$tmp/res/appfont.ttf" "$REPO/pkg/appfont.ttf" || { rm -rf "$tmp"; die "packaged res/appfont.ttf differs from pkg/appfont.ttf"; }
  rm -rf "$tmp"
  echo "package: $TPK ($(du -h "$TPK" | cut -f1))"
}

[ "$DO_SO" = 1 ] && build_so
[ "$DO_TPK" = 1 ] && build_tpk
say "done ($ARCH)"
[ "$DO_SO" = 1 ] && echo "  .so  $SO"
[ "$DO_TPK" = 1 ] && echo "  .tpk $TPK"
if [ "$DO_TPK" = 1 ]; then
  if [ -n "$LOG_HOST" ]; then
    echo "  log  the app sends its log to udp://$LOG_HOST (receive: nc -klu ${LOG_HOST##*:})"
  else
    echo "  log  no network log (no --log-host)"
  fi
  if [ -n "$MOCK_HOST" ]; then
    echo "  net  step 9 GETs http://$MOCK_HOST/identity (serve it: python3 tests/mock_pms.py --host 0.0.0.0 --port ${MOCK_HOST##*:})"
  else
    echo "  net  no mock host (no --mock-host): step 9 only reports whether libcurl loaded"
  fi
fi
if [ "$DO_SO" = 1 ]; then
  if [ "$NO_GFX" = 1 ]; then echo "  gfx  not built (--no-gfx): step 8 says so"; else echo "  gfx  plx_gfx linked in; step 8 runs the application's renderer"; fi
fi
exit 0
