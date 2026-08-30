#!/usr/bin/env bash
#
# abi-probe.sh — inspect one of the TV's OWN shared libraries from the host, so a new
# FFI binding can be proven before it is written.
#
#   abi-probe.sh pull  <soname-or-path>            fetch the library off the TV into the cache
#   abi-probe.sh syms  <lib> [regex]               exported symbols (demangled), or grep by regex
#   abi-probe.sh has   <lib> <sym> [sym...]        per-symbol PRESENT/ABSENT (exit 1 if any absent)
#   abi-probe.sh opts  <lib> <options-symbol>      AVOption table -> real struct offsets (FFmpeg)
#   abi-probe.sh info  <lib>                       SONAME, NEEDED, arch, build strings
#
# WHY THIS EXISTS: we link against hand-written stub .so files carrying the TV's real
# SONAMEs, so THE LINK ALWAYS SUCCEEDS — a symbol that does not exist on the device, or a
# struct offset taken from upstream headers that do not match this build, fails only at
# runtime, as a wrong value or a SIGSEGV, on a device with no debugger. Prove it here first.
# See .agents/skills/bind-tv-lib-abi/SKILL.md for the full procedure.
#
# The TV itself has no binutils (only `strings`), so every inspection runs HOST-side with
# the NDK's cross binutils against a pulled copy. Copies are cached in .abi-cache/
# (gitignored) and reused; `pull` refreshes.
#
# Config: TV host comes from $TV, else the Makefile's TV default. No credentials or host
# addresses are baked into this script.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE="$REPO/.abi-cache"
: "${WEBOS_SDK:=$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
TOOL="$WEBOS_SDK/bin/arm-webos-linux-gnueabi"

# TV address: explicit $TV wins, else whatever the Makefile declares (single source of truth).
tv_host() {
  if [ -n "${TV:-}" ]; then echo "$TV"; return; fi
  make -C "$REPO" -pn 2>/dev/null | sed -n 's/^TV *= *//p' | head -1
}
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
tv_ssh() { ssh "${SSH_OPTS[@]}" "root@$(tv_host)" "$@"; }

need_tools() {
  [ -x "$TOOL-nm" ] || { echo "ERROR: NDK binutils not found at $TOOL-* (set WEBOS_SDK, or run the setup-environment skill)" >&2; exit 2; }
}

# Resolve a name like "libavcodec.so.57" (or a full device path) to a cached local copy,
# pulling it on first use.
resolve() {
  local want="$1" local_path
  mkdir -p "$CACHE"
  # already cached? (match the soname prefix, newest wins)
  local_path="$(ls -t "$CACHE/$(basename "$want")"* 2>/dev/null | head -1 || true)"
  if [ -n "$local_path" ] && [ -f "$local_path" ]; then echo "$local_path"; return; fi
  pull_lib "$want"
}

pull_lib() {
  local want="$1" dev_path
  mkdir -p "$CACHE"
  case "$want" in
    /*) dev_path="$want" ;;
    # follow the SONAME symlink to the real versioned file so the cache records the build
    *)  dev_path="$(tv_ssh "readlink -f /usr/lib/$want 2>/dev/null || echo /usr/lib/$want")" ;;
  esac
  local base; base="$(basename "$dev_path")"
  echo "pulling $dev_path -> $CACHE/$base" >&2
  scp "${SSH_OPTS[@]}" -q "root@$(tv_host):$dev_path" "$CACHE/$base"
  echo "$CACHE/$base"
}

cmd="${1:-}"; shift || true
case "$cmd" in
  pull)
    need_tools; pull_lib "$1" ;;

  info)
    need_tools; lib="$(resolve "$1")"
    echo "== $lib"
    "$TOOL-readelf" -d "$lib" | grep -E 'SONAME|NEEDED' || true
    "$TOOL-readelf" -A "$lib" 2>/dev/null | grep -E 'Tag_CPU_arch|Tag_ABI_VFP|Tag_ABI_PCS' || true
    strings - "$lib" | grep -iE '^(FFmpeg|libav|configuration:|libjpeg|libcurl)' | head -5 || true ;;

  syms)
    need_tools; lib="$(resolve "$1")"; re="${2:-.}"
    "$TOOL-nm" -D --defined-only "$lib" | awk '{print $3}' | sed 's/@@*.*$//' | "$TOOL-c++filt" | grep -E "$re" | sort -u ;;

  has)
    need_tools; lib="$(resolve "$1")"; shift
    # dynamic symbols carry a version suffix (foo@@LIBAVCODEC_57) — strip it before matching
    exported="$("$TOOL-nm" -D --defined-only "$lib" | awk '{print $3}' | sed 's/@@*.*$//')"
    rc=0
    for s in "$@"; do
      if grep -qx -- "$s" <<<"$exported"; then echo "PRESENT  $s"
      else echo "ABSENT   $s"; rc=1; fi
    done
    exit $rc ;;

  opts)
    # FFmpeg AVOption tables embed the TRUE struct offset of every option-backed field for
    # THIS build — the most reliable offset source there is (upstream headers shift with the
    # FF_API_* deprecation flags and with ARM alignment padding). The TV's libraries are
    # STRIPPED, so we scan the read-only data sections for AVOption-shaped records instead of
    # looking up a table symbol. Prints: name  offset  type.  Optional regex filters by name.
    need_tools; lib="$(resolve "$1")"; filter="${2:-.}"
    python3 - "$lib" "$filter" "$TOOL" <<'PY'
import subprocess, sys, struct, re
lib, filt, tool = sys.argv[1], sys.argv[2], sys.argv[3]

def run(*a): return subprocess.run(a, capture_output=True, text=True).stdout

secs = []
for ln in run(f"{tool}-readelf", "-S", "-W", lib).splitlines():
    p = ln.replace("[", " ").replace("]", " ").split()
    if len(p) > 6 and p[1].startswith("."):
        try: secs.append((p[1], int(p[3], 16), int(p[4], 16), int(p[5], 16)))
        except ValueError: pass

blob = open(lib, "rb").read()
def read(vma, n):
    for _, addr, off, size in secs:
        if addr <= vma < addr + size:
            return blob[off + (vma - addr): off + (vma - addr) + n]
    return b""

# An AVOption NAME is a short lowercase identifier ("b", "g", "bufsize", "time_base").
# Help strings are prose and must NOT pass this, or every help pointer parses as a record.
NAME = re.compile(rb"^[a-z][a-z0-9_]{0,30}\x00")
def optname(vma):
    m = NAME.match(read(vma, 40))
    return m.group(0)[:-1].decode() if m else None

def printable(vma):
    b = read(vma, 200)
    if not b: return False
    t = b.split(b"\x00")[0]
    return len(t) > 0 and all(32 <= c < 127 for c in t)

# AVOption on 32-bit ARM: name*(+0) help*(+4) offset:int(+8) type:int(+12)
# default_val union(8B, 8-aligned)(+16) min:double(+24) max:double(+32) flags:int(+40) unit*(+44)
STRIDE = 48
# AVOptionType is NOT a plain 0..N enum in FFmpeg 3.x: CONST is 128 and the richer
# types are four-char MKBETAG codes. Numbering it sequentially (the shape you get from
# skimming a 4.x header) rejects every real entry and accepts garbage instead.
def _tag(s): return (ord(s[0]) << 24) | (ord(s[1]) << 16) | (ord(s[2]) << 8) | ord(s[3])
TYPES = {0:"FLAGS",1:"INT",2:"INT64",3:"DOUBLE",4:"FLOAT",5:"STRING",6:"RATIONAL",
         7:"BINARY",8:"DICT",9:"UINT64",128:"CONST",
         _tag("SIZE"):"IMAGE_SIZE", _tag("PFMT"):"PIXEL_FMT", _tag("SFMT"):"SAMPLE_FMT",
         _tag("VRAT"):"VIDEO_RATE", _tag("DUR "):"DURATION", _tag("COLR"):"COLOR",
         _tag("CHLA"):"CHANNEL_LAYOUT", _tag("BOOL"):"BOOL"}

def parse_entry(vma):
    e = read(vma, STRIDE)
    if len(e) < STRIDE: return None
    name_p, help_p, off, ty = struct.unpack("<IIiI", e[:16])
    if name_p == 0 or ty not in TYPES: return None
    if off < 0 or off > 8192 or off % 4: return None   # struct field offsets are 4-aligned
    name = optname(name_p)
    if not name: return None
    if help_p and not printable(help_p): return None
    # a CONST is a named value of the previous option (offset 0); anything else must
    # point at a real field
    if TYPES[ty] != "CONST" and off == 0: return None
    return (name, off, TYPES[ty])

# scan the data/rodata sections for runs of >=6 consecutive valid entries = a real table
found, seen = [], set()
for sname, addr, off, size in secs:
    if not any(k in sname for k in (".data.rel.ro", ".rodata", ".data")): continue
    for base in range(addr, addr + size - STRIDE, 4):
        if base in seen: continue
        run_entries, vma, addrs = [], base, []
        while True:
            ent = parse_entry(vma)
            if not ent: break
            run_entries.append(ent); addrs.append(vma); vma += STRIDE
            if len(run_entries) > 4096: break
        # only a run we ACCEPT consumes its addresses; a rejected short run must not
        # mark them, or it hides the real table that starts a few entries later
        if len(run_entries) >= 8:
            found.append(run_entries); seen.update(addrs)

rx = re.compile(filt)
print(f"{'option':28} {'offset':>7}  type")
print("-" * 48)
n = 0
for tbl in found:
    for name, off, ty in tbl:
        if ty == "CONST":      # a named VALUE of the previous option, not a struct field
            continue
        if rx.search(name):
            print(f"{name:28} {off:7}  {ty}"); n += 1
if n == 0:
    print(f"(no AVOption records matched /{filt}/ in {len(found)} table(s) found)")
elif "-v" in sys.argv:
    print(f"\n({len(found)} table(s) scanned)")
PY
    ;;

  *)
    sed -n '3,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2 ;;
esac
