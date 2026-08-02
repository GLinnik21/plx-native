#!/usr/bin/env bash
# Artifact assertions on the cross-built binary. Runs on any host — GNU readelf reads any ELF
# regardless of the host architecture.
#
# These exist because the stub-.so link trick (see the Makefile header) makes the LINK succeed
# whether or not a symbol or library is really on the TV. The first sign of trouble would
# otherwise be a rejected webosbrew PR, or a television that will not start.
set -euo pipefail

cd "$(dirname "$0")/.."
BIN="${1:-pkg/plxnative}"
# macOS ships no readelf; fall back to the NDK's, which is on any machine that can build this.
NDK_BIN="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}/bin"
if [ -z "${READELF:-}" ]; then
  if command -v readelf >/dev/null 2>&1; then READELF=readelf
  elif [ -x "$NDK_BIN/arm-webos-linux-gnueabi-readelf" ]; then READELF="$NDK_BIN/arm-webos-linux-gnueabi-readelf"
  else echo "::error::no readelf (install binutils, or set WEBOS_SDK/READELF)"; exit 1
  fi
fi
# llvm-objdump is multi-arch; GNU objdump on the host is not, so prefer the NDK's cross objdump.
if [ -z "${OBJDUMP:-}" ]; then
  if [ -x "$NDK_BIN/arm-webos-linux-gnueabi-objdump" ]; then OBJDUMP="$NDK_BIN/arm-webos-linux-gnueabi-objdump"
  else OBJDUMP=llvm-objdump
  fi
fi

fail() { echo "::error::$*"; exit 1; }
ok()   { echo "  ok — $*"; }

echo "== ELF identity =="
H=$("$READELF" -h "$BIN")
grep -q 'Class: *ELF32'      <<<"$H" || fail "not ELF32"
grep -q 'Machine: *ARM'      <<<"$H" || fail "not ARM"
grep -q 'Flags:.*soft-float' <<<"$H" || fail "not soft-float ABI (the NDK's softfp convention)"
"$READELF" -A "$BIN" | grep -q 'Tag_CPU_arch: v7' || fail "not ARMv7 (Tag_CPU_arch)"
ok "ELF32 / ARM / soft-float / ARMv7"

echo "== CP15 barrier regression =="
# The SIGILL bug: default arm-*-gnueabi (ARMv6) codegen emits `mcr p15,...,c7,c10,5`, which is
# UNDEFINED on the A53. rust-modules/.cargo/config.toml names the exact scenario that reintroduces
# it — "a future CI" that sets RUSTFLAGS in the environment, which REPLACES the config.toml list
# rather than appending, silently dropping -C target-cpu=cortex-a9.
if ! command -v "$OBJDUMP" >/dev/null 2>&1; then
  echo "  SKIP — $OBJDUMP not found (install llvm or set OBJDUMP=)"
else
  D=$("$OBJDUMP" -d "$BIN")
  CP15=$(grep -ciE 'mcr[[:space:]]+p?15,[[:space:]]*#?0,[[:space:]]*r[0-9]+,[[:space:]]*c(r)?7,[[:space:]]*c(r)?10' <<<"$D" || true)
  DMB=$(grep -cE '[[:space:]]dmb([[:space:]]|$)' <<<"$D" || true)
  [ "$CP15" -eq 0 ]  || fail "$CP15 CP15 barrier instructions — this binary will SIGILL on the TV"
  # Positive control. Without it a zero CP15 count could mean "clean" OR "objdump disassembled
  # nothing", and the check would pass vacuously forever.
  [ "$DMB" -gt 100 ] || fail "only $DMB dmb instructions — the disassembly scan is not working"
  ok "0 CP15 barriers, $DMB dmb (scan verified live)"
fi

echo "== DT_NEEDED =="
# Adding a call to any library that happens to be in the NDK sysroot silently adds a DT_NEEDED
# entry. webosbrew CI resolves this exact list against 14 firmware databases and rejects on a
# miss, so drift here is a rejected submission.
"$READELF" -d "$BIN" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | sort > /tmp/dt-needed.actual
if ! diff -u ci/expected-dt-needed.txt /tmp/dt-needed.actual; then
  fail "DT_NEEDED drifted. If intended, check the new library exists on webOS 4.x, then update ci/expected-dt-needed.txt"
fi
ok "$(wc -l < /tmp/dt-needed.actual | tr -d ' ') entries, unchanged"

echo "== build-host identity =="
# docs/distribution.md §4: a public build must not carry the developer's LAN or home directory.
# A clean CI checkout has no src/config.local.h, so src/app.h's __has_include falls back to the
# YOUR_PMS_HOST placeholder — which makes the CI-built binary the leak-free one by construction.
# On a dev machine config.local.h is present BY DESIGN, so this section is informational there and
# gating only where it can be satisfied: CI, which is the only thing that should build a release.
if [ -f src/config.local.h ] && [ "${CI:-}" != "true" ]; then
  echo "  SKIP — src/config.local.h present: this is a dev build, not a release artifact."
  echo "         (CI builds from a clean checkout, where this section gates.)"
  echo "all ELF assertions passed (build-host section skipped)"
  exit 0
fi
if strings -a "$BIN" | grep -qE '\b(10|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}\b'; then
  strings -a "$BIN" | grep -oE '\b(10|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}\b' | sort -u | head
  fail "private IP address baked into the binary — was this built with src/config.local.h present?"
fi
if strings -a "$BIN" | grep -qE '/Users/|/home/runner|/home/[a-z]'; then
  fail "build-host paths in the binary — release builds need -C --remap-path-prefix (see RELEASE=1)"
fi
strings -a "$BIN" | grep -q YOUR_PMS_HOST \
  || fail "YOUR_PMS_HOST placeholder absent — a real PMS_HOST was compiled in"
ok "no private IPs, no host paths, placeholder present"

echo "all ELF assertions passed"
