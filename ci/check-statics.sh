#!/usr/bin/env bash
# The statics gate of the UI restructure (spec §0 done-criterion 1, §15.2): `static mut` in a
# screen or engine module is ZERO except for render caches allowlisted BY NAME with a reason, and
# the three main-thread machine globals (`route/decision.rs SESSION`, `player/engine.rs ENGINE`,
# `ui/press.rs S`) are gone. `ui/press.rs S` went in phase 2; the other two go in phase 9.
#
# Two allowlists, both `# count: N` files that `tests/test_harness.py` audits (declared count ==
# entries, every path exists):
#   ci/allow/statics.txt           the PERMANENT list — `path<TAB>NAME: reason`, one entry per
#                                  static, matched by path AND name. A `Rect` static is never a
#                                  render cache (§15.2), so a hit rect cannot be entered here.
#   ci/allow/statics-migration.txt the MIGRATION list — `path<TAB>phase N: reason`, one entry per
#                                  legacy FILE, matched by path alone; it shrinks with each phase
#                                  and is EMPTY at phase 12. A stale entry (a path with no static
#                                  left) fails, so a deletion must also delete its entry.
# Every `static mut` under ui/, screens/, person.rs, metadata.rs, metadata/, pms.rs and stores/,
# plus the two named globals, must match one of the two. search/ is scanned with Required 1's
# wider spelling (`^\s*(pub(\(crate\))? )?static `, not just `static mut`) because that module
# holds `recents.rs`'s two `Mutex`-guarded persistence-seam statics (STORE, PENDING) — real
# process-wide state the narrower pattern cannot see. The wider pattern is NOT applied to the
# other gated directories: run tree-wide it also matches every `thread_local! { static … }`
# interior declaration (a different, safe, per-thread idiom used all over ui/ and screens/ for
# render caches and test doubles), which would surface on the order of 80 unrelated hits in one
# gate change. That widening is deliberately out of scope here — see the PR discussion — and
# must not be resolved by mass-allowlisting.
set -uo pipefail
cd "$(dirname "$0")/.."
SRC=rust-modules/src
fails=0
fail() { echo "::error::check-statics: $*"; fails=$((fails+1)); }
ok()   { echo "  ok — $*"; }

# The gated paths, checked to exist before every scan below: a renamed or moved module must not
# silently drop out of the gate the way `"$SRC/person"` (never a real path — Person is the file
# `person.rs`) used to. `2>/dev/null` on the grep itself is gone for the same reason: a missing
# path is now a loud gate failure, not a swallowed scan of nothing.
GATED_MUT_PATHS=("$SRC/ui" "$SRC/screens" "$SRC/person.rs" "$SRC/metadata.rs" "$SRC/metadata" "$SRC/pms.rs" "$SRC/stores")
GATED_WIDE_PATHS=("$SRC/search")
for p in "${GATED_MUT_PATHS[@]}" "${GATED_WIDE_PATHS[@]}" "$SRC/route/decision.rs" "$SRC/player/engine.rs" "$SRC/ui/press.rs"; do
  [ -e "$p" ] || fail "gated path missing: $p — the statics gate would silently scan nothing here"
done

# matches: `path<TAB>NAME` for every gated static declaration.
matches() {
  { grep -rnE --include='*.rs' '^\s*static mut [A-Za-z_][A-Za-z_0-9]*' "${GATED_MUT_PATHS[@]}"
    grep -rnE --include='*.rs' '^\s*(pub(\(crate\))? )?static (mut )?[A-Za-z_][A-Za-z_0-9]*' "${GATED_WIDE_PATHS[@]}"
    grep -nE '^\s*static mut SESSION\b' "$SRC/route/decision.rs" | sed "s|^|$SRC/route/decision.rs:|"
    grep -nE '^\s*static mut ENGINE\b' "$SRC/player/engine.rs" | sed "s|^|$SRC/player/engine.rs:|"
    grep -nE '^\s*static mut S\b' "$SRC/ui/press.rs" | sed "s|^|$SRC/ui/press.rs:|"
  } | sed -E "s/^([^:]+):[0-9]+:[[:space:]]*(pub(\(crate\))? )?static (mut )?([A-Za-z_][A-Za-z_0-9]*).*/\1	\5/" | sort -u
}

echo "== check-statics =="
all=$(matches)   # once: under pipefail a piped grep -q fails on the early close
perm=ci/allow/statics.txt
migr=ci/allow/statics-migration.txt
bad=0; n_perm=0; n_migr=0
while IFS=$'\t' read -r path name; do
  [ -z "$path" ] && continue
  if grep -qE "^${path}	${name}:" "$perm"; then n_perm=$((n_perm+1))
  elif grep -qE "^${path}	" "$migr"; then n_migr=$((n_migr+1))
  else echo "    $path: static mut $name"; bad=$((bad+1)); fi
done <<<"$all"
if [ "$bad" -eq 0 ]; then ok "statics: $n_perm named render cache(s), $n_migr in legacy modules awaiting their phase"
else fail "statics: $bad static mut(s) outside ci/allow/statics.txt and ci/allow/statics-migration.txt"; fi

# stale entries: an allowlisted static or file that no longer exists must leave the list.
stale=0
while IFS=$'\t' read -r path rest; do
  [ -z "$path" ] && continue
  name="${rest%%:*}"
  grep -qE "^${path}	${name}$" <<<"$all" || { echo "    stale: $path $name"; stale=$((stale+1)); }
done < <(grep -v '^#' "$perm")
while IFS=$'\t' read -r path rest; do
  [ -z "$path" ] && continue
  grep -qE "^${path}	" <<<"$all" || { echo "    stale: $path"; stale=$((stale+1)); }
done < <(grep -v '^#' "$migr")
if [ "$stale" -eq 0 ]; then ok "no stale allowlist entry"; else fail "$stale stale allowlist entr(ies) — delete them with the static"; fi

# the three machine globals: reported by name so the phase-9 deletion is visible here.
for g in "$SRC/route/decision.rs	SESSION" "$SRC/player/engine.rs	ENGINE" "$SRC/ui/press.rs	S"; do
  p="${g%%	*}"; n="${g##*	}"
  if grep -qE "^${p}	${n}$" <<<"$all"; then echo "    pending: $p $n (allowlisted under migration)"; else ok "global gone: $p $n"; fi
done

if [ "$fails" -eq 0 ]; then echo "check-statics: all gates green"; else echo "check-statics: $fails gate(s) red"; exit 1; fi
