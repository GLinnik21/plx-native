#!/usr/bin/env bash
# Structure gates for the UI restructure (spec §15.2), as greps over rust-modules/src. Each rule
# is either ZERO outside a named set of files or ALLOWLISTED by file in ci/allow/<rule>.txt, whose
# first line is `# count: N` — the number of entries — and whose entries are repo-relative paths
# with a reason after a tab. `tests/test_harness.py` runs this script and asserts every
# allowlist's count equals its entries, so an allowlist grows only by a deliberate edit of both.
#
# Phase 2 rules (the rest of §15.2 land with the phases that make them true):
#   libm     — the transcendental surface outside ui/motion.rs (spec §4.2): logical state must
#              integrate with motion.rs's own exp/sin_cos; a render or colour formula is allowlisted.
#   ticks    — `SDL_GetTicks(` only in app/clock.rs (the one door) and diag/heartbeat.rs.
#   fpflags  — no `fp-contract`, `fast-math` or `+fma` in the build configuration.
#   wall     — `Instant::now`/`SystemTime::now`/`.elapsed()` in ui/ and app/ only in instruments.
#   present  — the present gate's worker door: ONE atomic static in ui/present.rs and ONE
#              `wake_from_worker`.
#   effect   — `Effect::` spelled nowhere (the enum is `Fx::`, the app's `AppFx::`).
#
# Phase 4 rule:
#   mutators — a screen (ui/, screens/) or the loop (app/) never calls a data module's MUTATOR directly
#              (`crate::browse::set_cur(`, `crate::search::set_query(`, …): every mutation is a
#              `stores::StoreCmd` applied through `stores::<store>::apply` (spec §14, the
#              (caller, mutator) allowlist — `docs/stores-as-machines.md`). PRODUCTION lines only:
#              a `#[cfg(test)] mod` seeds a store however it likes. The player side (route.rs,
#              player/) joins in phase 9.
set -uo pipefail
cd "$(dirname "$0")/.."
SRC=rust-modules/src
fails=0
fail() { echo "::error::check-deps: $*"; fails=$((fails+1)); }
ok()   { echo "  ok — $*"; }

# grep_code <pattern> <paths...>: matching lines, minus comment-only lines, as `path:line:text`.
grep_code() {
  local pat="$1"; shift
  grep -rnE --include='*.rs' "$pat" "$@" 2>/dev/null | grep -vE '^[^:]+:[0-9]+:\s*//' || true
}

# allowed <rule> <path>: is `path` an entry of ci/allow/<rule>.txt?
allowed() {
  local rule="$1" path="$2"
  grep -qE "^${path}(	|$)" "ci/allow/${rule}.txt" 2>/dev/null
}

# gate <rule> <pattern> <paths...>: every match must be in an allowlisted file.
gate() {
  local rule="$1" pat="$2"; shift 2
  local bad=0
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    local p="${line%%:*}"
    if ! allowed "$rule" "$p"; then echo "    $line"; bad=$((bad+1)); fi
  done < <(grep_code "$pat" "$@")
  if [ "$bad" -eq 0 ]; then ok "$rule"; else fail "$rule: $bad line(s) outside ci/allow/$rule.txt"; fi
}

echo "== check-deps =="
# libm: the method-call spelling, OUTSIDE ui/motion.rs (which owns the integrators and their
# table test); `.log(&…`/`.log("…` is a logger, not a logarithm.
libm_lines=$(grep_code '\.(exp|ln|log|powf|powi|cbrt|sin|cos|tan|atan2|hypot|mul_add|sin_cos)\(' "$SRC" \
  | grep -vE '\.log\((&|")' | grep -v "^$SRC/ui/motion.rs:")
libm_bad=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  p="${line%%:*}"
  if ! allowed libm "$p"; then echo "    $line"; libm_bad=$((libm_bad+1)); fi
done <<< "$libm_lines"
if [ "$libm_bad" -eq 0 ]; then ok "libm"; else fail "libm: $libm_bad line(s) outside ci/allow/libm.txt"; fi

gate ticks 'SDL_GetTicks\(' "$SRC"
gate wall '(Instant::now|SystemTime::now|\.elapsed\(\))' "$SRC/ui" "$SRC/app"

if grep -rnE 'fp-contract|fast-math|\+fma' rust-modules/Cargo.toml rust-modules/build.rs rust-modules/.cargo Makefile 2>/dev/null | grep -v '^\s*#'; then
  fail "fpflags: a floating-point contraction flag is set (spec §4.2 assumes none)"
else ok "fpflags"; fi

n=$(grep -cE '^static [A-Z_]+: Atomic' "$SRC/ui/present.rs"); d=$(grep -c 'pub fn wake_from_worker' "$SRC/ui/present.rs")
if [ "$n" -eq 1 ] && [ "$d" -eq 1 ]; then ok "present: one worker door"; else fail "present: $n atomic statics, $d doors (one of each)"; fi

if [ -n "$(grep_code '\bEffect::' "$SRC")" ]; then fail "effect: \`Effect::\` is spelled (use Fx:: / AppFx::)"; else ok "effect"; fi

# mutators: production lines of ui/, screens/ and app/ (everything before the file's first
# `#[cfg(test)]` + `mod` pair, which is where every screen keeps its tests).
#
# **`screens/` joined this list in phase 5b and that was not cosmetic.** The gate's own rule is
# "a SCREEN never calls a data module's mutator directly", and until 5b every screen lived under
# `ui/`, so scanning `ui/` and `app/` scanned every screen there was. The migration moves screens
# to `rust-modules/src/screens/` one family at a time — so from the moment the first one landed,
# the gate was silently blind to exactly the code it exists to police, and a migrated screen could
# call `browse::apply_pins(` with the gate still reporting green. It is the same shape as the hole
# the comment below records (cutting at the FIRST test module and leaving ~700 lines unscanned):
# a gate that passes because it looked at the wrong thing reads identical to one that passes
# because the code is clean.
MUTATORS='\b(browse|pms|metadata|search|person|viewstate)::(set_cur|note_library_choice|kick_letters|kick_genres|want|save_view|set_sort_by_key|set_sort|toggle_unwatched|set_genre_by_id|set_genre|retry_cur_source|recheck_shares|apply_pins|toggle_pin|retry_discovery|reset|discover_pump|pump|pump_detail|pump_season|pump_alt_sources|request|open|close|set_query|request_detail|load_detail_now|clear|load_season|load_season_now|set_now_playing|set_watched_local|install_playing|mark_skipped|retire_playing|retire_playing_item|request_refetch_hubs|request_retry|edit_item)\(|\bsection_hubs::(kick|commit_staged|invalidate_all|invalidate|set_watched_local|left_the_deck)\('
# The spelling is matched WITHOUT a `crate::` prefix (a `use crate::metadata;` makes it
# `metadata::load_season(`), and every `#[cfg(test)] mod … { … }` block is skipped by brace depth
# wherever it sits in the file — the first version cut at the FIRST such block and let ~700
# production lines of ui/detail.rs go unscanned. `stores::<store>::apply(` lines are the new
# spelling and are excluded by name; a SCREEN's own `crate::ui::person::open(` is not a store
# call and is masked before the match.
mut_bad=0
while IFS= read -r f; do
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    if ! allowed mutators "$f"; then echo "    $f:$line"; mut_bad=$((mut_bad+1)); fi
  done < <(awk '
    skip>0 { n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m; if (depth<=0) skip=0; prev=$0; next }
    prev=="#[cfg(test)]" && /^mod / { skip=1; depth=gsub(/\{/,"{")-gsub(/\}/,"}"); if (depth<=0) skip=0; prev=$0; next }
    { print NR":"$0; prev=$0 }' "$f" | sed -E 's/crate::ui::[a-z_]+::[a-z_]+\(/UI_CALL(/g' | grep -E "$MUTATORS" | grep -vE '^[0-9]+:\s*//' | grep -v 'stores::' || true)
done < <(find "$SRC/ui" "$SRC/screens" "$SRC/app" -name '*.rs' | sort)
if [ "$mut_bad" -eq 0 ]; then ok "mutators"; else fail "mutators: $mut_bad line(s) call a store mutator directly (use stores::<store>::apply)"; fi

# every allowlist's declared count equals its entries
for f in ci/allow/*.txt; do
  declared=$(sed -n 's/^# count: *//p' "$f" | head -1)
  entries=$(grep -cvE '^\s*(#|$)' "$f")
  [ "$declared" = "$entries" ] || fail "$f declares count $declared but has $entries entries"
done

[ "$fails" -eq 0 ] && { echo "check-deps: all gates green"; exit 0; }
echo "check-deps: $fails gate(s) failed"; exit 1
