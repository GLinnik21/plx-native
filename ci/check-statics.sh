#!/usr/bin/env bash
# The statics gate of the UI restructure (spec §0 done-criterion 1, §15.2): `static mut` in a
# screen or engine module is ZERO except for process-wide statics allowlisted BY NAME with a reason
# (a frame-loop render cache is a `thread_local!`, never an entry), and
# the three main-thread machine globals (`route/decision.rs SESSION`, `player/engine.rs ENGINE`,
# `ui/press.rs S`) are gone. `ui/press.rs S` went in phase 2; the other two go in phase 9.
#
# Two allowlists, both `# count: N` files that `tests/test_harness.py` audits (declared count ==
# entries, every path exists):
#   ci/allow/statics.txt           the PERMANENT list — `path<TAB>NAME: reason`, one entry per
#                                  static, matched by path AND name. A `Rect` static is never a
#                                  process-wide static (§15.2), so a hit rect cannot be entered here.
#   ci/allow/statics-migration.txt the MIGRATION list — `path<TAB>phase N: reason`, one entry per
#                                  legacy FILE, matched by path alone; it shrinks with each phase
#                                  and is EMPTY at phase 12. A stale entry (a path with no static
#                                  left) fails, so a deletion must also delete its entry.
# Every `static mut` under ui/, appkit/, screens/, (in the data crate) person.rs, metadata.rs, metadata/, pms.rs and stores/,
# plus the two named globals, must match one of the two. search/ is scanned with Required 1's
# wider spelling (`^\s*(pub(\(crate\))? )?static `, not just `static mut`) because that module
# holds `recents.rs`'s two `Mutex`-guarded persistence-seam statics (STORE, PENDING) — real
# process-wide state the narrower pattern cannot see. The wider pattern is NOT applied to the
# other gated directories: run tree-wide it also matches every `thread_local! { static … }`
# interior declaration (a different, safe, per-thread idiom used all over ui/ and screens/ for
# render caches and test doubles), which would surface on the order of 80 unrelated hits in one
# gate change. That widening is deliberately out of scope here — see the PR discussion — and
# must not be resolved by mass-allowlisting.
#
# One rule is TREE-WIDE (every crate under rust-modules, `ci/check-statics-heap.py`): a `static
# mut` whose TYPE OWNS HEAP (a Vec, HashMap, String, Box, Rc or CString, directly or through a
# struct it names) is refused unless ci/allow/statics.txt lists it by path and NAME with a reason.
# Such a static is replaced, cleared or inserted into, and that frees memory, so two threads that
# do it to the one static free each other's data; `cargo test` runs tests on parallel threads, so
# the bug is a random crash in a stranger's run (`plx_ui`'s TAB_CACHE, 2026-10-20). A frame-loop
# cache is a `thread_local!`; a heap static that is genuinely process-wide earns a named entry
# that says which single thread owns it or what serialises it (`ci/check-ledger-ratchet.py` fails a
# PR that grows ci/allow/statics.txt, so a new entry is the owner's call). Plain Copy statics (GL object
# names and uniform locations, function pointers, raw FFI handles, flags, counters, times, NonZero*,
# atomics and Cell-family wrappers of plain data) stay outside the rule: they free nothing when
# written. A static a `macro_rules!` declares is listed under the macro's name and `!` (`cache!`).
# The rule reads `static mut` only; a non-mut `UnsafeCell<heap>` made Sync by an `unsafe impl` is
# the gap `ci/check-statics-heap.py` documents.
set -uo pipefail
cd "$(dirname "$0")/.."
SRC=rust-modules/src
# The ui layer is its own crate (docs/module-layers.md, Split 6); its files were $SRC/ui.
UI=rust-modules/ui/src
# The data layer is its own crate (docs/module-layers.md, Split 9); its files were $SRC/stores, $SRC/pms.rs,
# $SRC/metadata*, $SRC/person.rs and $SRC/search.
DATA=rust-modules/data/src
# The media layer is its own crate (docs/module-layers.md, Split 11); its files were $SRC/route and $SRC/player.
MEDIA=rust-modules/media/src
# The appkit layer is its own crate (docs/module-layers.md, Split 12); its files were $SRC/appkit.
APPKIT=rust-modules/appkit/src
# The screens layer is its own crate (docs/module-layers.md, Split 13); its files were $SRC/screens.
SCREENS=rust-modules/screens/src
fails=0
fail() { echo "::error::check-statics: $*"; fails=$((fails+1)); }
ok()   { echo "  ok — $*"; }

# The gated paths, checked to exist before every scan below: a renamed or moved module must not
# silently drop out of the gate the way `"$SRC/person"` (never a real path — Person is the file
# `person.rs`) used to. `2>/dev/null` on the grep itself is gone for the same reason: a missing
# path is now a loud gate failure, not a swallowed scan of nothing.
GATED_MUT_PATHS=("$UI" "$APPKIT" "$SCREENS" "$DATA/person.rs" "$DATA/metadata.rs" "$DATA/metadata" "$DATA/pms.rs" "$DATA/stores")
GATED_WIDE_PATHS=("$DATA/search")
for p in "${GATED_MUT_PATHS[@]}" "${GATED_WIDE_PATHS[@]}" "$MEDIA/route/decision.rs" "$MEDIA/player/engine.rs" "$UI/press.rs"; do
  [ -e "$p" ] || fail "gated path missing: $p — the statics gate would silently scan nothing here"
done

# matches: `path<TAB>NAME` for every gated static declaration.
matches() {
  { grep -rnE --include='*.rs' '^\s*static mut [A-Za-z_][A-Za-z_0-9]*' "${GATED_MUT_PATHS[@]}"
    grep -rnE --include='*.rs' '^\s*(pub(\(crate\))? )?static (mut )?[A-Za-z_][A-Za-z_0-9]*' "${GATED_WIDE_PATHS[@]}"
    grep -nE '^\s*static mut SESSION\b' "$MEDIA/route/decision.rs" | sed "s|^|$MEDIA/route/decision.rs:|"
    grep -nE '^\s*static mut ENGINE\b' "$MEDIA/player/engine.rs" | sed "s|^|$MEDIA/player/engine.rs:|"
    grep -nE '^\s*static mut S\b' "$UI/press.rs" | sed "s|^|$UI/press.rs:|"
  } | sed -E "s/^([^:]+):[0-9]+:[[:space:]]*(pub(\(crate\))? )?static (mut )?([A-Za-z_][A-Za-z_0-9]*).*/\1	\5/" | sort -u
}

echo "== check-statics =="
all=$(matches)   # once: under pipefail a piped grep -q fails on the early close
# path<TAB>NAME<TAB>type — why. A crashed detector must be red, not an empty list the loop below calls ok.
heap=$(python3 -I ci/check-statics-heap.py) || fail "ci/check-statics-heap.py failed — the heap rule below read nothing"
heap_keys=$(cut -f1,2 <<<"$heap")
perm=ci/allow/statics.txt
migr=ci/allow/statics-migration.txt
bad=0; n_perm=0; n_migr=0
while IFS=$'\t' read -r path name; do
  [ -z "$path" ] && continue
  if grep -qE "^${path}	${name}:" "$perm"; then n_perm=$((n_perm+1))
  elif grep -qE "^${path}	" "$migr"; then n_migr=$((n_migr+1))
  else echo "    $path: static mut $name"; bad=$((bad+1)); fi
done <<<"$all"
if [ "$bad" -eq 0 ]; then ok "statics: $n_perm named static(s), $n_migr in legacy modules awaiting their phase"
else fail "statics: $bad static mut(s) outside ci/allow/statics.txt and ci/allow/statics-migration.txt"; fi

# the tree-wide heap rule: a heap-owning `static mut` outside the gated directories (those were
# judged above) must be named in the permanent list.
bad=0; n_heap=0
while IFS=$'\t' read -r path name why; do
  [ -z "$path" ] && continue
  grep -qE "^${path}	${name}$" <<<"$all" && continue
  if grep -qE "^${path}	${name}:" "$perm"; then n_heap=$((n_heap+1))
  else echo "    $path: static mut $name — $why"; bad=$((bad+1)); fi
done <<<"$heap"
if [ "$bad" -eq 0 ]; then ok "heap-owning static mut outside the gated directories: $n_heap named in ci/allow/statics.txt, none unlisted"
else fail "statics: $bad heap-owning static mut(s) not in ci/allow/statics.txt — make it a thread_local! (a frame-loop cache) or name it there with the thread that owns it (ci/check-ledger-ratchet.py fails a PR that grows the list, so a new entry is the owner's call); if the type is plain data the detector misread, fix the detector (ci/check-statics-heap.py), do not allowlist it"; fi

# stale entries: an allowlisted static or file that no longer exists must leave the list.
known=$(printf '%s\n%s\n' "$all" "$heap_keys")
stale=0
while IFS=$'\t' read -r path rest; do
  [ -z "$path" ] && continue
  name="${rest%%:*}"
  grep -qE "^${path}	${name}$" <<<"$known" || { echo "    stale: $path $name"; stale=$((stale+1)); }
done < <(grep -v '^#' "$perm")
while IFS=$'\t' read -r path rest; do
  [ -z "$path" ] && continue
  grep -qE "^${path}	" <<<"$all" || { echo "    stale: $path"; stale=$((stale+1)); }
done < <(grep -v '^#' "$migr")
if [ "$stale" -eq 0 ]; then ok "no stale allowlist entry"; else fail "$stale stale allowlist entr(ies) — delete them with the static"; fi

# the three machine globals: reported by name so the phase-9 deletion is visible here.
for g in "$MEDIA/route/decision.rs	SESSION" "$MEDIA/player/engine.rs	ENGINE" "$UI/press.rs	S"; do
  p="${g%%	*}"; n="${g##*	}"
  if grep -qE "^${p}	${n}$" <<<"$all"; then echo "    pending: $p $n (allowlisted under migration)"; else ok "global gone: $p $n"; fi
done

if [ "$fails" -eq 0 ]; then echo "check-statics: all gates green"; else echo "check-statics: $fails gate(s) red"; exit 1; fi
