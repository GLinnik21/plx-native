#!/usr/bin/env bash
# Build the step-1 Samsung Tizen "hello world": libplx.so (Rust) and the .NET .tpk that carries it.
#
#   tizen/hello/build.sh [--arch arm|x86_64|i686] [--so-only | --tpk-only]
#
#   --arch arm      (default) real TVs, 32-bit ARMv7 softfp. Built on this Mac with the webOS NDK's
#                   gcc and glibc-2.12 sysroot (the same NDK the app uses).
#   --arch x86_64   the Tizen TV emulator, TV Extension 10.0 (its only image is x86_64). Built on a
#                   LINUX box (the tester's WSL2) with plain host cargo and the host gcc.
#   --arch i686     the older 32-bit 9.0 emulator image. Linux only; needs a 32-bit gcc:
#                   `apt install gcc-i686-linux-gnu` (or gcc-multilib).
#   --so-only       build libplx.so and verify it; skip the .NET step (WSL without dotnet).
#   --tpk-only      package an existing out/<arch>/libplx.so (build the .so elsewhere first).
#
# Output (all gitignored):
#   tizen/hello/out/<arch>/libplx.so       the library
#   tizen/hello/out/PlxHello-<arch>.tpk    the package (default developer certificate; the tester
#                                          re-signs per TV with Apps2Samsung. No certificate is
#                                          ever committed.)
#
# Environment (all optional):
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
#                        older glibc (Debian buster, or a Tizen rootstrap). Passed as --sysroot. UNTESTED.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PLX="$HERE/plx"
HOST="$HERE/host/PlxHello"
OUT="$HERE/out"

ARCH=arm
DO_SO=1
DO_TPK=1
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="${2:?--arch needs arm, x86_64 or i686}"; shift 2 ;;
    --arch=*) ARCH="${1#--arch=}"; shift ;;
    --so-only) DO_TPK=0; shift ;;
    --tpk-only) DO_SO=0; shift ;;
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
mkdir -p "$OUT/$ARCH"

say() { printf '\n== %s\n' "$*"; }
die() { echo "build.sh: $*" >&2; exit 1; }

# --- the Rust library ---------------------------------------------------------------------
build_so() {
  export PATH="$HOME/.cargo/bin:$PATH"
  # Per the repo's rules for hand-run cargo: no incremental cache, target dir inside the tree
  # (gitignored), never /tmp.
  export CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PLX/target"

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

  local plus=""; [ -z "$toolchain" ] || plus="+$toolchain"
  # shellcheck disable=SC2086
  if ! rustup $plus target list --installed 2>/dev/null | grep -qx "$TRIPLE"; then
    say "rustup target add $TRIPLE ${toolchain:+(toolchain $toolchain)}"
    rustup target add ${toolchain:+--toolchain "$toolchain"} "$TRIPLE"
  fi

  say "cargo ${plus} build --release --target $TRIPLE"
  # shellcheck disable=SC2086
  cargo $plus build --release --manifest-path "$PLX/Cargo.toml" --target "$TRIPLE"
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
  local want got
  want="$(grep -hoE 'plx_tizen_[a-z]+' "$HOST/Program.cs" | sort -u)"
  got="$("$nm" -D --defined-only "$SO" | awk '{print $3}')"
  for f in $want; do grep -qx "$f" <<<"$got" || die "$SO does not export $f"; done

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

  say "dotnet build -c Release (Tizen.NET.Sdk, tizen80)"
  rm -f "$HOST"/bin/Release/tizen80/*.tpk
  (cd "$HOST" && dotnet build PlxHello.csproj -c Release -nologo)
  local built; built="$(ls "$HOST"/bin/Release/tizen80/*.tpk 2>/dev/null | head -1 || true)"
  [ -n "$built" ] || die "dotnet produced no .tpk"
  cp "$built" "$TPK"

  say "verify $TPK"
  local listing; listing="$(unzip -l "$TPK")"
  echo "$listing"
  for need in 'tizen-manifest.xml' 'bin/PlxHello.dll' 'lib/libplx.so'; do
    grep -q " $need\$" <<<"$listing" || die "$TPK lacks $need"
  done
  # The library in the package must be byte-identical to the one that was verified.
  local tmp; tmp="$(mktemp -d)"
  unzip -qo "$TPK" lib/libplx.so -d "$tmp"
  cmp -s "$tmp/lib/libplx.so" "$SO" || { rm -rf "$tmp"; die "packaged libplx.so differs from $SO"; }
  rm -rf "$tmp"
  echo "package: $TPK ($(du -h "$TPK" | cut -f1))"
}

[ "$DO_SO" = 1 ] && build_so
[ "$DO_TPK" = 1 ] && build_tpk
say "done ($ARCH)"
[ "$DO_SO" = 1 ] && echo "  .so  $SO"
[ "$DO_TPK" = 1 ] && echo "  .tpk $TPK"
exit 0
