#!/usr/bin/env bash
# decomp.sh — decompile a function out of one of the TV's own ARM32 libraries.
#
# The television's media stack is closed, stripped and undocumented, so the only way to
# answer "what does this firmware actually do with that field" is to read the code. This
# wraps Ghidra's headless analyzer so a question costs one command instead of a setup.
#
#   decomp.sh pull                       harvest the media stack off the TV into the lab
#   decomp.sh list                       what has been harvested
#   decomp.sh syms <lib> [pattern]       exported symbols (dynsym), optionally filtered
#   decomp.sh str  <lib> <pattern>       .rodata strings — the cheap first pass
#   decomp.sh fn   <lib> <pattern> [n]   DECOMPILE matching functions (default 6)
#   decomp.sh xref <lib> <string>        which functions reference a string literal
#
# <lib> is a substring of a harvested file name (e.g. "playerAPIs", "libpf", "cbe").
#
# Analysis is cached per library: the first `fn` on a library pays ~15-60s, every later
# query on it is seconds. `decomp.sh clean` drops the cache.
set -euo pipefail

LAB="${DECOMP_LAB:-/tmp/tvlab}"
BIN="$LAB/bin"; PROJ="$LAB/ghidra-proj"; SCRIPTS="$LAB/scripts"

# --- toolchain ---------------------------------------------------------------------
# Homebrew installs openjdk keg-only (not on PATH) and ghidra as a FORMULA, not a cask —
# `brew install --cask ghidra` fails with "No Cask with this name exists".
JAVA_HOME="${JAVA_HOME:-$(ls -d /opt/homebrew/opt/openjdk@21 2>/dev/null || true)}"
export JAVA_HOME
[ -n "$JAVA_HOME" ] && export PATH="$JAVA_HOME/bin:$PATH"
AH="${GHIDRA_HEADLESS:-$(ls -d /opt/homebrew/Cellar/ghidra/*/libexec/support/analyzeHeadless 2>/dev/null | tail -1 || true)}"

NDK="$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi"
NM="$( [ -x "$NDK-nm" ] && echo "$NDK-nm" || echo nm )"
STRINGS="$( [ -x "$NDK-strings" ] && echo "$NDK-strings" || echo strings )"

die() { echo "decomp: $*" >&2; exit 1; }

need_ghidra() {
  [ -n "$AH" ] && [ -x "$AH" ] || die "Ghidra headless not found.
  brew install ghidra openjdk@21      # ghidra is a FORMULA, not a cask
  ...or set GHIDRA_HEADLESS=/path/to/analyzeHeadless"
  [ -n "$JAVA_HOME" ] || die "no JDK. Ghidra is a Java application; macOS /usr/bin/java is a
  stub that only offers to install one. brew install openjdk@21"
}

pick() {  # resolve a library substring to one harvested path
  local m; m=$(ls "$BIN" 2>/dev/null | grep -i -- "$1" || true)
  [ -n "$m" ] || die "no harvested library matches '$1'. Run: decomp.sh pull   (then: list)"
  [ "$(echo "$m" | wc -l)" -eq 1 ] || die "'$1' matches several:
$m"
  echo "$BIN/$m"
}

ensure_script() {
  mkdir -p "$SCRIPTS"
  # Java, not Python: Ghidra 12 dropped Jython, and PyGhidra needs a Python env we do not
  # want to require. Headless compiles a .java script on the fly.
  cat > "$SCRIPTS/DecompMatch.java" <<'JAVA'
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionIterator;
import ghidra.util.task.ConsoleTaskMonitor;

/** Decompile every function whose (mangled) name contains arg0. arg1 = cap, default 6. */
public class DecompMatch extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] a = getScriptArgs();
        String pat = a.length > 0 ? a[0].toLowerCase() : "";
        int cap = a.length > 1 ? Integer.parseInt(a[1]) : 6;
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        FunctionIterator it = currentProgram.getFunctionManager().getFunctions(true);
        int n = 0;
        while (it.hasNext() && n < cap) {
            Function f = it.next();
            if (!f.getName().toLowerCase().contains(pat)) continue;
            n++;
            println("======== " + f.getEntryPoint() + "  " + f.getName());
            DecompileResults r = di.decompileFunction(f, 180, new ConsoleTaskMonitor());
            println(r != null && r.decompileCompleted()
                ? r.getDecompiledFunction().getC() : "// DECOMPILE FAILED");
        }
        println("MATCHED=" + n + " for '" + pat + "'");
    }
}
JAVA
  cat > "$SCRIPTS/XrefString.java" <<'JAVA'
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Data;
import ghidra.program.model.listing.DataIterator;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;

/** Which functions reference a string literal containing arg0. */
public class XrefString extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] a = getScriptArgs();
        String pat = a.length > 0 ? a[0].toLowerCase() : "";
        DataIterator di = currentProgram.getListing().getDefinedData(true);
        int hits = 0;
        while (di.hasNext()) {
            Data d = di.next();
            Object v = d.getValue();
            if (!(v instanceof String)) continue;
            String s = (String) v;
            if (!s.toLowerCase().contains(pat)) continue;
            hits++;
            println("---- \"" + s + "\" @ " + d.getAddress());
            for (Reference r : getReferencesTo(d.getAddress())) {
                Address from = r.getFromAddress();
                Function f = getFunctionContaining(from);
                println("        from " + from + (f != null ? "  in " + f.getName() : "  (no function)"));
            }
        }
        println("STRINGS_MATCHED=" + hits + " for '" + pat + "'");
    }
}
JAVA
}

imported() {  # is <name> already analysed in the cached project?
  [ -d "$PROJ" ] && grep -qs -- "$1" "$PROJ"/*.rep/project.prp 2>/dev/null && return 0
  [ -d "$PROJ/smoke.rep" ] && find "$PROJ" -name "*${1}*" -maxdepth 4 2>/dev/null | grep -q . && return 0
  return 1
}

run_script() {  # <libpath> <script.java> <args...>
  need_ghidra; ensure_script
  local lib="$1" script="$2"; shift 2
  local name; name=$(basename "$lib")
  mkdir -p "$PROJ"
  if ! imported "$name"; then
    echo "decomp: analysing $name (first query on this library; later ones are fast)…" >&2
    "$AH" "$PROJ" tvlab -import "$lib" -analysisTimeoutPerFile 1800 >/dev/null 2>&1 \
      || die "Ghidra import failed for $name"
  fi
  "$AH" "$PROJ" tvlab -process "$name" -noanalysis -scriptPath "$SCRIPTS" \
        -postScript "$script" "$@" 2>&1 \
    | sed -n 's/^INFO  [A-Za-z]*\.java> //p'
}

cmd="${1:-help}"; shift || true
case "$cmd" in
  pull)
    TV="${TV_HOST:-${TV:-$(cat "$(dirname "$0")/../../../.tv-host" 2>/dev/null || true)}}"
    [ -n "$TV" ] || die "no TV host (.tv-host, or TV_HOST=)"
    mkdir -p "$BIN"
    echo "decomp: harvesting the media stack from $TV …" >&2
    # shellcheck disable=SC2087
    ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "root@$TV" \
      'find / -name "*.so*" -type f 2>/dev/null | grep -iE "player|/libpf|acb|cbe|smp|starfish|umedia|vpq|dile|dolby"' \
      > "$LAB/paths.txt" || die "ssh failed (TV asleep? .agents/skills/wake-tv/wake-tv.sh)"
    while read -r p; do
      [ -n "$p" ] || continue
      scp -q -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "root@$TV:$p" "$BIN/" 2>/dev/null || true
    done < "$LAB/paths.txt"
    ( cd "$BIN" && shasum -a 256 ./* > "$LAB/MANIFEST.txt" 2>/dev/null || true )
    echo "decomp: $(ls "$BIN" | wc -l | tr -d ' ') files in $BIN (sha256 in MANIFEST.txt)"
    ;;
  list)   ls -lh "$BIN" 2>/dev/null || die "nothing harvested yet — decomp.sh pull" ;;
  syms)   l=$(pick "${1:?lib}"); "$NM" -D --defined-only "$l" | { [ -n "${2:-}" ] && grep -i -- "$2" || cat; } ;;
  str)    l=$(pick "${1:?lib}"); "$STRINGS" -a "$l" | grep -i -- "${2:?pattern}" | sort -u ;;
  fn)     l=$(pick "${1:?lib}"); run_script "$l" DecompMatch.java "${2:?pattern}" "${3:-6}" ;;
  xref)   l=$(pick "${1:?lib}"); run_script "$l" XrefString.java "${2:?string}" ;;
  clean)  rm -rf "$PROJ" "$SCRIPTS"; echo "decomp: analysis cache dropped (harvested binaries kept)" ;;
  *)      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
