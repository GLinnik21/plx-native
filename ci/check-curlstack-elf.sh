#!/usr/bin/env bash
# ELF assertions on the opt-in bundled curl stack (`make CURLSTACK=1`): the seven `-plx` libraries
# `ci/build-curlstack.sh` builds and the package ships beside the binary. `ci/check-elf.sh` grades
# the application binary; these libraries run in the same process, so two of its properties have
# to hold for them as well, and one property is theirs alone:
#
#   * ISA level: ELF32 / ARM / soft-float ABI / `Tag_CPU_arch: v7`, and no CP15 barrier
#     (`mcr p15, 0, rX, c7, c10, …`), the ARMv6 encoding that SIGILLs on these SoCs
#     (`ci/check-elf.sh`, "CP15 barrier regression"). OpenSSL's `linux-armv4` assembly carries ARMv8
#     Crypto Extension code, but it emits those instructions as raw words selected at run time
#     from `OPENSSL_armcap_P` and keeps the object attributes at v7, so it passes this unchanged;
#     no allowance is needed, and a library whose attributes ever say v8 fails here.
#   * `exit`: the application exports its own `exit` (`src/exit.c`, the Makefile's
#     EXIT_INTERPOSER_LDFLAG) so that a library's `exit` ends the process through the crash log.
#     Every stack library is opened `RTLD_DEEPBIND` (`net::curlstack`), which resolves its own
#     undefined symbols against its own dependency chain, libc included, BEFORE the executable, so
#     an `exit` it imported would bind to libc's and bypass the interposer. None imports it today;
#     this fails the build the day one does, so the exception is decided rather than discovered.
#
# Usage: ci/check-curlstack-elf.sh [lib ...]   (default: the Makefile's CURLSTACK=1 staged list)
set -euo pipefail

cd "$(dirname "$0")/.."
NDK_BIN="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}/bin"
if [ -z "${READELF:-}" ]; then
  if command -v readelf >/dev/null 2>&1; then READELF=readelf
  elif [ -x "$NDK_BIN/arm-webos-linux-gnueabi-readelf" ]; then READELF="$NDK_BIN/arm-webos-linux-gnueabi-readelf"
  else echo "::error::no readelf (install binutils, or set WEBOS_SDK/READELF)"; exit 1
  fi
fi
if [ -z "${OBJDUMP:-}" ]; then
  if [ -x "$NDK_BIN/arm-webos-linux-gnueabi-objdump" ]; then OBJDUMP="$NDK_BIN/arm-webos-linux-gnueabi-objdump"
  else OBJDUMP=llvm-objdump
  fi
fi
command -v "$OBJDUMP" >/dev/null 2>&1 || { echo "::error::$OBJDUMP not found (install llvm or set OBJDUMP=)"; exit 1; }

fail() { echo "::error::$*"; exit 1; }
ok()   { echo "  ok — $*"; }

if [ "$#" -gt 0 ]; then
  LIBS=("$@")
else
  read -r -a LIBS <<<"$(make -s print-curlstack-staged CURLSTACK=1)"
fi
[ "${#LIBS[@]}" -gt 0 ] || fail "no curl-stack libraries to check"

echo "== bundled curl stack: ISA level, CP15 barriers, exit =="
for lib in "${LIBS[@]}"; do
  [ -f "$lib" ] || fail "$lib is missing"
  H=$("$READELF" -h "$lib") || fail "readelf -h failed on $lib"
  grep -q 'Class: *ELF32'      <<<"$H" || fail "$lib is not ELF32"
  grep -q 'Machine: *ARM'      <<<"$H" || fail "$lib is not ARM"
  grep -q 'Flags:.*soft-float' <<<"$H" || fail "$lib is not the soft-float ABI (the NDK's softfp convention)"
  A=$("$READELF" -A "$lib") || fail "readelf -A failed on $lib"
  grep -q 'Tag_CPU_arch: v7$' <<<"$A" || fail "$lib is not Tag_CPU_arch v7: $(grep 'Tag_CPU_arch:' <<<"$A" | head -1)"
  D=$("$OBJDUMP" -d "$lib") || fail "objdump -d failed on $lib"
  CP15=$(grep -ciE 'mcr[[:space:]]+p?15,[[:space:]]*#?0,[[:space:]]*r[0-9]+,[[:space:]]*c(r)?7,[[:space:]]*c(r)?10' <<<"$D" || true)
  [ "$CP15" -eq 0 ] || fail "$lib: $CP15 CP15 barrier instructions — it will SIGILL on the TV"
  # Positive control: a zero count must mean "clean", not "objdump disassembled nothing". Every
  # library has thousands of `bx`/`pop`/`ldr` lines; 100 is far below any of them and far above noise.
  INSNS=$(grep -cE '^[[:space:]]+[0-9a-f]+:[[:space:]]' <<<"$D" || true)
  [ "$INSNS" -gt 100 ] || fail "$lib: only $INSNS disassembled instructions — the scan is not working"
  DYN=$("$READELF" --dyn-syms -W "$lib") || fail "readelf --dyn-syms failed on $lib"
  if awk '$7 == "UND" { sub(/@.*/, "", $8); if ($8 == "exit") found=1 } END { exit !found }' <<<"$DYN"; then
    fail "$lib imports exit; under RTLD_DEEPBIND it binds libc's and bypasses src/exit.c — decide and record the exception beside EXIT_INTERPOSER_LDFLAG in the Makefile"
  fi
  ok "$(basename "$lib"): ELF32 / ARM / soft-float / ARMv7, 0 CP15 barriers in $INSNS instructions, no exit import"
done
echo "all curl-stack ELF assertions passed"
