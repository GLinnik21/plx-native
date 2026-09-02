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

echo "== Starfish callback interposer =="
# libplayerAPIs' fixed libpf thunk calls this exact C++ symbol through an R_ARM_JUMP_SLOT. A normal
# executable-private definition is absent from .dynsym and therefore cannot preempt that slot; the
# link must export exactly this one symbol. Conversely, Load-with-context is resolved and owner-
# checked with dlsym so a firmware which lacks the overload refuses playback instead of killing the
# app in the dynamic loader before main.
SMP_HOOK='_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc'
SMP_LOAD_CTX='_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_'
DYN_SYMS=$($READELF --dyn-syms -W "$BIN")
HOOK_ROWS=$(awk -v symbol="$SMP_HOOK" '$NF == symbol { print }' <<<"$DYN_SYMS")
[ "$(wc -l <<<"$HOOK_ROWS" | tr -d ' ')" -eq 1 ] \
  || fail "callbackFunctionHook interposer is not present exactly once in .dynsym"
awk '$4 == "FUNC" && $5 == "GLOBAL" && $6 == "DEFAULT" && $7 != "UND" { ok=1 } \
     END { exit !ok }' <<<"$HOOK_ROWS" \
  || fail "callbackFunctionHook interposer is not a GLOBAL/DEFAULT .dynsym function"
if awk -v symbol="$SMP_LOAD_CTX" '$NF == symbol { found=1 } END { exit !found }' <<<"$DYN_SYMS"; then
  fail "Load-with-context is a dynamic symbol dependency; it must be dlsym'd for firmware fallback"
fi
if "$READELF" -rW "$BIN" | grep -q "$SMP_LOAD_CTX"; then
  fail "Load-with-context has a dynamic relocation; missing firmware would fail before main"
fi
ok "exact hook exported GLOBAL/DEFAULT; Load-with-context has no loader dependency"

echo "== crash-report identity =="
# Two facts a Sentry crash report is built out of, and BOTH fail silently when wrong: a frame comes
# back `symbolicatorStatus: "missing_symbol"` with no error attached, which is indistinguishable
# from never having uploaded symbols at all. There is no way to notice from the dashboard, so they
# are asserted here, on the artifact.
#
# (1) The IMAGE BASE. Symbolicator computes `rva = instruction_addr - image_addr`, and for this
# non-PIE executable that base is the lowest PT_LOAD vaddr rather than 0. The app derives it from
# its own program headers at runtime (`telemetry::sentry::image_addr`), so this is not what the
# report USES — it is the check that the fallback constant, and the value measured against the live
# project on 2026-08-29, still describe the link.
grep -q 'Type: *EXEC' <<<"$H" || fail "no longer ET_EXEC — the image base is now a load bias (telemetry::sentry::image_addr returns None for ET_DYN and would fall back to a WRONG constant)"
# One variable for the expectation, named once: written twice, a change to the comparison and a
# change to the message drift apart, and the failure then reports "is X, not X".
WANT_BASE=0x00010000
LOAD_BASE=$("$READELF" -l "$BIN" | awk '/^  LOAD/{print $3}' | LC_ALL=C sort | head -1)
[ "$LOAD_BASE" = "$WANT_BASE" ] \
  || fail "lowest PT_LOAD is $LOAD_BASE, not $WANT_BASE — update telemetry::sentry::IMAGE_ADDR and re-verify that a real crash still symbolicates"

# (2) The BUILD ID. It is the only thing that pairs a stripped binary a stranger's television
# faulted in with the pkg/plxnative.debug a release uploaded. `-Wl,--build-id=sha1` is
# unconditional on every link and `strip` preserves it, so an absent one means the flag was lost.
"$READELF" -n "$BIN" | grep -qi 'Build ID: *[0-9a-f]\{40\}' \
  || fail "no 40-hex GNU build id — -Wl,--build-id=sha1 was lost, and nothing can then pair a \
crash report with its symbols"
ok "ET_EXEC, image base $LOAD_BASE, sha1 build id present"

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
# LC_ALL=C is load-bearing, and its absence is what made this gate fail on its first CI run while
# passing on every dev machine. `sort` collates by LOCALE: macOS's default UTF-8 locale orders
# case-insensitively (…libgcc_s, libGLESv2, libglib…) while the Linux runner's C locale puts every
# capital first (libAcbAPI, libGLESv2, libSDL2…, then libav…). Same 21 libraries either way — the
# diff was pure ordering, which reads exactly like the ABI drift this check exists to catch.
# The expectation file is regenerated in C collation to match.
"$READELF" -d "$BIN" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | LC_ALL=C sort > /tmp/dt-needed.actual
if ! diff -u ci/expected-dt-needed.txt /tmp/dt-needed.actual; then
  fail "DT_NEEDED drifted. If intended, confirm with tools/fwcompat.py that it exists on every supported release, then update ci/expected-dt-needed.txt"
fi
ok "$(wc -l < /tmp/dt-needed.actual | tr -d ' ') entries, unchanged"

echo "== build-host identity =="
# docs/distribution.md §4: a public build must not carry the developer's LAN or home directory.
# Those are TWO independent properties and this section used to conflate them, which is how the
# one that gates everything went unverified for so long:
#
#   the LAN address comes from src/config.local.h, which is present on a dev machine BY DESIGN
#   (src/app.h's __has_include falls back to YOUR_PMS_HOST only in a clean checkout) — so those
#   assertions genuinely can only hold in CI;
#
#   the HOME DIRECTORY has nothing to do with config.local.h. It arrives through `-Z build-std`
#   compiling std and every dependency from absolute paths under $RUSTUP_HOME/$CARGO_HOME, and
#   it is fixed by the Makefile's --remap-path-prefix. That check can and should run everywhere.
#
# Skipping the whole section on a dev machine meant the host-path gate had never once executed
# against a real build. It also cannot be left CI-only now: the remap is the thing that makes the
# ipk's byte-for-byte reproducibility claim true, and a dev build is where it would be broken.
if strings -a "$BIN" | grep -qE '(^|[^[:alnum:]/_.-])/(Users|home)/[a-z]'; then
  strings -a "$BIN" | grep -oE '(^|[^[:alnum:]/_.-])/(Users|home)/[a-z][^ ]*' | sort -u | head
  fail "build-host paths in the binary — the build must set --remap-path-prefix (see the Makefile's RUST_REMAP)"
fi
# The pattern is anchored to a path BOUNDARY rather than matching '/home/[a-z]' anywhere, because
# the plex.tv endpoint '/api/v2/home/users' is a substring match and would fail every build.
ok "no build-host paths"

if [ -f src/config.local.h ] && [ "${CI:-}" != "true" ]; then
  echo "  SKIP (private-IP + placeholder) — src/config.local.h present: a dev build, not a release."
  echo "         (CI builds from a clean checkout, where these gate.)"
  echo "all ELF assertions passed (config-dependent assertions skipped)"
  exit 0
fi
if strings -a "$BIN" | grep -qE '\b(10|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}\b'; then
  strings -a "$BIN" | grep -oE '\b(10|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}\b' | sort -u | head
  fail "private IP address baked into the binary — was this built with src/config.local.h present?"
fi
strings -a "$BIN" | grep -q YOUR_PMS_HOST \
  || fail "YOUR_PMS_HOST placeholder absent — a real PMS_HOST was compiled in"
ok "no private IPs, placeholder present"

echo "all ELF assertions passed"
