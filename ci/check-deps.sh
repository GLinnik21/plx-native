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
# Phase 4 rule (D3 rewrite, phase 12):
#   mutators — a screen (ui/, screens/) or the loop (app/) never calls a data module's MUTATOR directly
#              (`crate::browse::set_cur(`, `crate::search::set_query(`, …): every mutation is a
#              `stores::StoreCmd` applied through `stores::<store>::apply` (spec §14, the
#              (caller, mutator) allowlist — `docs/stores-as-machines.md`). PRODUCTION lines only:
#              a `#[cfg(test)] mod` seeds a store however it likes. The player side joins in phase
#              9: `route/` (both halves of the split — `plan.rs`, the pure selection half, and
#              `decision.rs`, the network/adapter half) and `player/` are scanned the same as
#              `screens/`/`app/` (zero hits at the split, so green on day one; the `wall` rule below
#              is the one that distinguishes the halves: it gates `route/plan.rs` and exempts
#              `route/decision.rs`). Phase 10 adds `dev/`: the dev-trigger arms that used to live
#              in `app/{boot,run,content,mod}.rs` moved to `dev/scenarios.rs`, and a mutator call
#              that moved with them must not launder itself out of this gate's scan. **This was a
#              CALL-SITE gate alone through phase 10, and D3's census (2026-09-10/11) found the
#              hole that shape leaves**: it can only ever prove "nobody currently calls this
#              directly", never "nobody CAN" — a mutator sitting `pub(crate)` and unreached today
#              is one accidental `use` away from a violation the gate would then have to catch by
#              name a second time. `mutators-visibility` (below) is the fix: it reads the
#              DECLARATION line of every real mutator in its owning legacy module and fails if it
#              is anything looser than private/`pub(super)`, so `stores::<store>::apply` (or, for
#              `browse::section_hubs`, a `pub(super)` reached only from its parent `browse`) is
#              the only door BY CONSTRUCTION, not by nobody having tried the other one yet. The
#              two gates are independent and both must be green: a name absent from
#              `mutators-visibility`'s per-file list (a PUMP/landing door like `pump`/`tick`/
#              `land`/`discover_pump`/`take_detail_refresh`, which stays `pub(crate)` BY DESIGN as
#              the sanctioned door between a store and its owning module — see
#              `docs/stores-as-machines.md`) can still be caught calling FROM a screen by the
#              call-site half, and a name whose declaration IS private can still, in principle, be
#              wrapped by a same-file door that leaks it back out — which the call-site half would
#              catch on ITS spelling, not the original's.
#
# Phase 8 rule (§14, §6.2):
#   nav      — a screen under rust-modules/src/screens/ never calls `crate::ui::nav::` (any
#              function) live; it reads `DrawFrame::{page_alpha,chrome_alpha,view_tab,
#              blur_amount,nav_page_alpha}`, populated once per frame by `app/bridge.rs`'s
#              `Rig::navigation_presentation`. `ui/`'s CONTAINERS (popover.rs, glassload.rs,
#              widgets.rs's chrome helpers, the loop's own app/nav.rs and app/run.rs) still call
#              `ui::nav` directly — that is phase 7/12's boundary, not this one's; this gate
#              scans only `screens/`, where the count is zero.
# Phase 10 rules:
#   layer    — a screen (`screens/`) never names `crate::app::`/`super::app::` (§2.1's table). The
#              other half of that table has been gated since phase 2 (`ui/` names no application
#              type); this half was prose until the argument and the mounter moved into
#              `screens/registry.rs`, which is the boundary §0 criterion 5 stands on. Zero, no
#              allowlist.
#   legacypage — `LegacyPage` is spelled NOWHERE under rust-modules/src (§15.2). The type was a
#              route WORD wearing the `Screen` trait, mounted by `AppArg::Legacy`'s fallback arm,
#              and by phase 9 nothing constructed one — every route mounted an owned screen. A
#              fallback nothing takes is not free: it is what stopped the mounter's match from
#              being exhaustive, so a `Route` added without a screen compiled and mounted a blank
#              page instead of failing to build. Zero, with no allowlist: there is no such thing as
#              a legitimate second one.
#   sibling  — a file under `screens/<a>/` or `screens/<a>.rs` never names `crate::screens::<b>`
#              for a different `<b>` (§2.1, §0 criterion 2): a screen talks to the shared
#              vocabulary (`crate::screens::registry`, which the gate allows by name) and to the
#              container, never to its neighbours — reaching into a sibling is what makes a screen
#              impossible to mount, test or delete on its own. Existing violations are
#              ci/allow/sibling-migration.txt, one per FILE, and that list is empty when the
#              criterion is met. The own-module name is not a violation, and `mod.rs`/a submodule
#              of `screens/<a>/` counts as `<a>`.
#   sessionwrite — a screen (screens/, ui/) never calls `plex::session::load(`. `load` is the
#              BOOT/auth door: it mints a `client_id` when there is none and re-persists a
#              plaintext session, so a read turns into `write_atomic` — a temp file, `sync_all`,
#              a rename and a second `sync_all` on the directory. The read-only door is `peek`.
#              `session.rs` has said "it is not [an acceptable trade] on a path a keypress can
#              reach" and "do not add a per-frame reader of this file" since the two doors were
#              split, and a screen still had it: `screens/settings.rs::signed_in` asked the write
#              door twice per Settings open, which on a television with no usable key manager is
#              two flash writes and four fsyncs on the frame the modal mounts — 150-180 ms of
#              `navcommit` when the flash was slow (`fps:modal-ramp`, device-measured 2026-09-09).
#              Nothing failed: both doors return a `Session` and the difference is invisible at the
#              call site, which is exactly what a grep gate is for. Count is zero.
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

# strip_strings_and_comments <file>: prints the file, one line per input line (so line numbers of
# the output line up with `sed -n '<n>p'` on the original), with the CONTENT of every
# double-quoted string literal blanked to spaces (quotes kept, so `"foo"` becomes `"   "`, an
# escaped character inside a string counts as one blanked character pair) and everything from an
# unquoted `//` to end of line dropped. Used by gates (`frame`, `tmppath`) that must not fire on a
# call SHAPE that only appears as message text or as a self-test's own expected-string literal —
# `ui/idle.rs` compares against `app/run.rs`'s source as a string, which is exactly that shape.
# This is character-by-character rather than a same-line regex heuristic for the reason both those
# gates' own comments give: a `//` or a `"` that is itself inside a string must not end the scan
# early, and a multi-token call spelled across a `"..."` boundary must not be reassembled by luck.
# Deliberately not general Rust lexing (raw strings, byte strings, char literals) — every case
# these two gates exist for is an ordinary `"…"` literal or a plain line comment.
strip_strings_and_comments() {
  awk '
  {
    line = $0; out = ""; instr = 0; i = 1; n = length(line)
    while (i <= n) {
      c = substr(line, i, 1)
      if (instr) {
        if (c == "\\") { out = out "  "; i += 2; continue }
        if (c == "\"") { instr = 0; out = out c; i++; continue }
        out = out " "; i++; continue
      }
      if (c == "\"") { instr = 1; out = out c; i++; continue }
      if (c == "/" && substr(line, i+1, 1) == "/") { break }
      out = out c; i++
    }
    print out
  }' "$1"
}

# allowed <rule> <path>: is `path` an entry of ci/allow/<rule>.txt?
allowed() {
  local rule="$1" path="$2"
  grep -qE "^${path}(	|$)" "ci/allow/${rule}.txt" 2>/dev/null
}

# wholly_test_files: paths (under $SRC) that carry NO #[cfg(test)] marker of their own but are
# entirely test code anyway — the mutators call-site gate's per-file brace-depth skip only ever
# looks INSIDE the file it is scanning, so a file like this reads as 100% production to it.
# **Derived, never hand-listed** — a transcribed list rots the moment a new one is added and
# nothing here compiles Bash comments (D3 census finding). Two shapes:
#   (i)  a bare `#[cfg(test)]` immediately followed by `mod <name>;` — a DECLARATION with no body,
#        naming a test module split into its own sibling file (`screens/detail/mod.rs:14`'s
#        `mod tests;` names `screens/detail/tests.rs`).
#   (ii) `include!("<name>.rs")` found while walking INSIDE a `#[cfg(test)] mod { … }` block
#        (`app/bridge.rs`'s own test module `include!`s `detail_panel_tests.rs` and friends).
# Plain POSIX awk (no gawk `match(...,arr)` — this runs under BSD/one-true-awk too).
wholly_test_files() {
  find "$SRC" -name '*.rs' | while IFS= read -r f; do
    local dir; dir=$(dirname "$f")
    awk -v dir="$dir" '
      /^#\[cfg\(test\)\][ \t]*$/ { prevcfg=1; next }
      prevcfg==1 && /^mod [a-z_]+;/ {
        line=$0; sub(/^mod /,"",line); sub(/;.*/,"",line)
        print dir "/" line ".rs"
      }
      { prevcfg=0 }
    ' "$f" | while IFS= read -r cand; do
      [ -f "$cand" ] && echo "$cand"
    done
    awk '
      skip>0 {
        n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m
        print
        if (depth<=0) skip=0
        prev=$0; next
      }
      prev=="#[cfg(test)]" && /^mod / {
        skip=1; depth=gsub(/\{/,"{")-gsub(/\}/,"}")
        if (depth<=0) skip=0
        prev=$0; next
      }
      { prev=$0 }
    ' "$f" | grep -oE 'include!\("[^"]+"\)' | sed -E 's/include!\("([^"]+)"\)/\1/' | while IFS= read -r inc; do
      [ -f "$dir/$inc" ] && echo "$dir/$inc"
    done
  done | sort -u
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
#   wall (widened phase 12, D4): the scope grows from ui/+app/+route/plan.rs to also cover
#              screens/ and stores/ (screens/player/ is a subdirectory of screens/ and so already
#              included) — every screen migrated out of ui/ carries the same "instrument only"
#              rule its old home had. Re-verified clean on 2026-09-10 with no new violation.
gate wall '(Instant::now|SystemTime::now|\.elapsed\(\))' "$SRC/ui" "$SRC/app" "$SRC/route/plan.rs" "$SRC/screens" "$SRC/stores"

if grep -rnE 'fp-contract|fast-math|\+fma' rust-modules/Cargo.toml rust-modules/build.rs rust-modules/.cargo Makefile 2>/dev/null | grep -v '^\s*#'; then
  fail "fpflags: a floating-point contraction flag is set (spec §4.2 assumes none)"
else ok "fpflags"; fi

n=$(grep -cE '^static [A-Z_]+: Atomic' "$SRC/ui/present.rs"); d=$(grep -c 'pub fn wake_from_worker' "$SRC/ui/present.rs")
if [ "$n" -eq 1 ] && [ "$d" -eq 1 ]; then ok "present: one worker door"; else fail "present: $n atomic statics, $d doors (one of each)"; fi

if [ -n "$(grep_code '\bEffect::' "$SRC")" ]; then fail "effect: \`Effect::\` is spelled (use Fx:: / AppFx::)"; else ok "effect"; fi

# mutators: production lines of ui/, screens/ and app/ (everything before the file's first
# `#[cfg(test)]` + `mod` pair, which is where every screen keeps its tests) — PLUS, since D3, every
# file `wholly_test_files` names above, which carries no such marker of its own but is entirely
# test code (see that function's doc): the per-file brace-depth skip below cannot see that from
# inside the file, so without this a mutator call moved into one of those files (as `alt_install(`
# was, into `screens/alt_sources_tests.rs`, before this rule existed) reads as a hit on a file the
# gate scores 100% production.
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
#
# D3 (2026-09-11) regenerated this list against the real fn names — the previous one named
# `save_view`/`toggle_unwatched`, neither of which the code has ever spelled that way (the real
# names are `save_cursor`/`set_unwatched`), and `load_detail_now` for a function D1 deleted
# outright — and added every real mutator the census found missing entirely: `alt_install`,
# `alt_restamp_owners`, `alt_prune_inactive`, `alt_stand_in`, `record_pins`, `retry_source`,
# `apply_landing`, `take_detail_refresh`.
MUTATORS='\b(browse|pms|metadata|search|person|viewstate)::(set_cur|note_library_choice|kick_letters|kick_genres|want|save_cursor|set_sort_by_key|set_sort|set_unwatched|set_genre_by_id|set_genre|retry_cur_source|retry_source|recheck_shares|apply_pins|toggle_pin|record_pins|retry_discovery|reset|discover_pump|pump|pump_detail|pump_season|pump_alt_sources|request|open|close|set_query|request_detail|clear|load_season|load_season_now|set_now_playing|set_watched_local|install_playing|mark_skipped|retire_playing|retire_playing_item|alt_install|alt_restamp_owners|alt_prune_inactive|alt_stand_in|request_refetch_hubs|request_retry|edit_item|apply_landing|take_detail_refresh)\(|\bsection_hubs::(kick|commit_staged|invalidate_all|invalidate|set_watched_local|left_the_deck)\('
# The spelling is matched WITHOUT a `crate::` prefix (a `use crate::metadata;` makes it
# `metadata::load_season(`), and every `#[cfg(test)] mod … { … }` block is skipped by brace depth
# wherever it sits in the file — the first version cut at the FIRST such block and let ~700
# production lines of ui/detail.rs go unscanned. `stores::<store>::apply(` lines are the new
# spelling and are excluded by name; a SCREEN's own `crate::ui::person::open(` is not a store
# call and is masked before the match.
mut_wholly_test="$(wholly_test_files)"
mut_bad=0
while IFS= read -r f; do
  if echo "$mut_wholly_test" | grep -qxF "$f"; then continue; fi
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    if ! allowed mutators "$f"; then echo "    $f:$line"; mut_bad=$((mut_bad+1)); fi
  done < <(awk '
    skip>0 { n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m; if (depth<=0) skip=0; prev=$0; next }
    prev=="#[cfg(test)]" && /^mod / { skip=1; depth=gsub(/\{/,"{")-gsub(/\}/,"}"); if (depth<=0) skip=0; prev=$0; next }
    { print NR":"$0; prev=$0 }' "$f" | sed -E 's/crate::ui::[a-z_]+::[a-z_]+\(/UI_CALL(/g' | grep -E "$MUTATORS" | grep -vE '^[0-9]+:\s*//' | grep -v 'stores::' || true)
done < <(find "$SRC/ui" "$SRC/screens" "$SRC/app" "$SRC/route" "$SRC/player" "$SRC/dev" -name '*.rs' | sort)
if [ "$mut_bad" -eq 0 ]; then ok "mutators"; else fail "mutators: $mut_bad line(s) call a store mutator directly (use stores::<store>::apply)"; fi

# mutators-visibility (D3): the call-site rule above can only ever prove "nobody currently calls
# this directly" — it says nothing about whether they COULD. This reads the DECLARATION line of
# every real mutator in its OWNING legacy module (one file, not a tree-wide name search: `open`,
# `close`, `clear`, `reset`, `request` are common enough method names elsewhere in the crate that
# a name-only tree scan would drown in unrelated `impl` methods) and fails if it is anything
# looser than private. `browse::section_hubs`'s five are `pub(super)`, genuinely tighter than
# private-to-crate-root since its parent is `browse`, a real module — the regex below only matches
# a bare `pub`/`pub(crate)`, so a `pub(super)` declaration is correctly invisible to it.
#
# Deliberately EXCLUDED, both documented here rather than silently absent:
#   - PUMP/landing doors (`pump`, `tick`, `land`, `discover_pump`, `take_detail_refresh`) stay
#     `pub(crate)` BY DESIGN — the sanctioned door between a store and the module `stores::<store>`
#     wraps, not a mutator a screen has any business calling (`docs/stores-as-machines.md`).
#   - `metadata::alt_stand_in` — the census flagged it as "MUTATOR-adjacent" by name alone; it
#     touches no crate-global state at all, a pure `Vec<AltCopy>` builder
#     `screens/alt_sources_tests.rs` calls directly to grade its own shape.
# No other exceptions: `pms::reset` (once tracked as an open item — `app/bridge.rs` and
# `app/recorder.rs` still called it directly from their own `#[cfg(test)] mod`s) is closed, routed
# through `stores::hubs::apply(HubsCmd::Reset)` (the variant and `pms::run`'s arm both already
# existed) and narrowed to private like every other `pms.rs` mutator.
# One "<file>|<space-separated fn list>" entry per store — a plain array, not `declare -A`: the
# script's own shebang is `env bash` and the dev Mac's `/bin/bash` is 3.2 (Apple ships nothing
# newer over the GPLv3 boundary), which has no associative arrays at all.
MUT_FNS_TABLE=(
  "browse/mod.rs|set_cur note_library_choice kick_letters kick_genres want save_cursor set_sort_by_key set_sort set_unwatched set_genre_by_id set_genre retry_cur_source retry_source recheck_shares apply_pins toggle_pin record_pins retry_discovery reset set_watched_local"
  "browse/section_hubs.rs|kick commit_staged invalidate_all invalidate set_watched_local left_the_deck"
  "metadata.rs|request_detail clear load_season load_season_now set_now_playing set_watched_local install_playing mark_skipped retire_playing retire_playing_item alt_install alt_restamp_owners alt_prune_inactive"
  "pms.rs|request_refetch_hubs request_retry edit_item apply_landing reset"
  "search.rs|set_query reset set_watched_local"
  "person.rs|open close reset set_watched_local"
  "viewstate.rs|request reset"
)
# store-seams <relf> <fn>: is `relf::fn` an entry of ci/allow/store-seams.txt? That file keys by
# `<path>::<fn>` rather than by path alone (unlike every other allowlist's `allowed()`, which
# would exempt a whole file's worth of mutators for one worker-seam fn) — worker-thread landing
# seams only, per that file's own header.
store_seamed() {
  grep -qE "^${1}::${2}(	|$)" ci/allow/store-seams.txt 2>/dev/null
}
vis_bad=0
for entry in "${MUT_FNS_TABLE[@]}"; do
  relf="${entry%%|*}"
  fns="${entry#*|}"
  f="$SRC/$relf"
  for fn in $fns; do
    hit=$(grep -nE "^[[:space:]]*pub(\(crate\))?[[:space:]]+fn[[:space:]]+${fn}\b" "$f" 2>/dev/null || true)
    if [ -n "$hit" ] && ! store_seamed "$relf" "$fn"; then
      echo "    $relf: fn $fn is still pub(crate)/pub — narrow to private (stores::<store>::apply must be the only door)"
      vis_bad=$((vis_bad+1))
    fi
  done
done
if [ "$vis_bad" -eq 0 ]; then ok "mutators-visibility"; else fail "mutators-visibility: $vis_bad fn(s) still crate-visible"; fi

gate nav 'crate::ui::nav::' "$SRC/screens"

# layer: a SCREEN never names the application. §2.1's table says `screens/` may name `ui/`,
# `stores/`, `plex/` and `player/` and never `app/`, and until phase 10 that half of the rule was
# prose alone — which is exactly how `screens/registry.rs` came to record, in its own module doc,
# that it could not hold the concrete `ScreenArg` because the argument carried an `app`-private
# `Route`. The type moved and the rule is now a grep, because the criterion that depends on it (§0
# criterion 5: a new screen touches its own file, the registry, `dev/scenarios.rs` and the manifest
# and nothing else) is only worth as much as the boundary underneath it. Count is zero, with no
# allowlist: a screen that needs something of the loop's asks for it as an effect (`AppFx`,
# `LoopReq`) — that is what the bundle in `registry.rs` is.
if [ -n "$(grep_code '(crate|super)::app::' "$SRC/screens")" ]; then
  grep_code '(crate|super)::app::' "$SRC/screens" | sed 's/^/    /'
  fail "layer: a screen names the application (§2.1) — ask for it as an AppFx/LoopReq instead"
else ok "layer"; fi
gate sessionwrite 'session::load\(' "$SRC/screens" "$SRC/ui"

# legacypage: the word itself, anywhere under src — a doc that still describes the type is as much
# a hit as a declaration, which is the point (nothing compiles the prose either).
legacy_hits=$(grep -rn --include='*.rs' 'LegacyPage' "$SRC" 2>/dev/null || true)
if [ -z "$legacy_hits" ]; then ok "legacypage"; else
  echo "$legacy_hits" | sed 's/^/    /'
  fail "legacypage: $(echo "$legacy_hits" | wc -l | tr -d ' ') mention(s) — the type is retired (§15.2)"
fi

# sibling: one screen family per directory; `<a>` is the first path component under screens/.
#
# `screens/registry.rs` is EXEMPT, and by design rather than by allowlist: it holds the concrete
# `ScreenArg` and the one `mount` match (§2.1), so naming every screen is the whole of its job —
# that match is the single place the application says which argument mounts which screen, and a
# gate that forbade it would forbid the structure the spec asks for. It was an allowlist entry
# until phase 10 moved the mounter into it; an entry would have had to say "this file names all of
# them, permanently", which is a rule and not a migration.
sib_bad=0
while IFS= read -r f; do
  [ "$f" = "$SRC/screens/registry.rs" ] && continue
  rel="${f#"$SRC"/screens/}"
  own="${rel%%/*}"
  own="${own%.rs}"
  hits=$(grep_code 'crate::screens::[a-z_]+' "$f" | grep -oE 'crate::screens::[a-z_]+' | sort -u \
    | grep -vE "^crate::screens::(registry|${own})$" || true)
  [ -z "$hits" ] && continue
  if ! allowed sibling-migration "$f"; then
    echo "    $f: $(echo "$hits" | tr '\n' ' ')"
    sib_bad=$((sib_bad+1))
  fi
done < <(find "$SRC/screens" -name '*.rs' | sort)
if [ "$sib_bad" -eq 0 ]; then ok "sibling"
else fail "sibling: $sib_bad file(s) name a sibling screen (use crate::screens::registry)"; fi

# ============================================================================================
# Phase 12 rules (spec §15.2, D4): the gates the earlier phases' own comments above never wrote,
# added here because their preconditions were supposed to have landed by the time this package
# ran. Three of them (route, ladder, hittest — plus the `#[cfg(test)] mod`/`run`-length checks
# below) did NOT find that precondition true when they were WRITTEN, and were landed red on
# purpose: a red result was the accurate signal that D1 (Route/Nav/Trail retirement) was not done,
# not a bug in the gate. **All of them are green now** — PX-OVERLAYS closed `ladder`/`hittest` and
# PX-D1 deleted the `Route` enum, `app/nav.rs` and `ui/trail.rs`, which is what `route`, `fnlen` and
# `testmod` were each waiting on. They stay BLOCKING, which is the whole point: the retirement is
# only finished for as long as nothing re-introduces the shape.
# ============================================================================================

# textmeasure (phase 12, D4 — ZERO now, was an allowlist): crate::text::(text_width|elide|cap_h)
# outside the three files that own the raw primitives (`text.rs`, `ui/text_view.rs`,
# `ui/text_buffer.rs` — the TextView component built directly on them) and the BODY of an
# `impl … Measure for …` block. That second exemption is structural, not a path: `Measure`
# (`ui/machine.rs`) is the one seam (`TtfMeasure` on device/sim, `TableMeasure` under replay,
# `FixtureMeasure` in host tests), and every real call site now threads it down from its caller —
# but two leaves genuinely could not (each struct's own doc says which and why): `widgets.rs`'s
# `LegacyMeasure` (generic `View::draw` leaves with no capability parameter, and the legacy
# tab-row cache reached only through `app::bridge`, off-limits to this lane) and `login.rs`'s
# `RawTextMeasure` (a host-test-only stand-in for `TtfMeasure`, whose `width` carries a boot-order
# `debug_assert!` that a test with no `init_text` would trip). Both wrap the identical free
# functions `TtfMeasure` does, minus the assert, so detecting the block structurally — rather than
# allowlisting the two files outright — is what keeps this a real zero-tolerance gate: a THIRD
# `impl Measure for` added later still only exempts ITS OWN body, and any other raw call anywhere
# in the tree fails.
tm_seams="$SRC/ui/text_view.rs $SRC/ui/text_buffer.rs $SRC/text.rs"
tm_bad=0
while IFS= read -r f; do
  is_seam=0
  for s in $tm_seams; do [ "$f" = "$s" ] && is_seam=1; done
  [ "$is_seam" -eq 1 ] && continue
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    echo "    $f:$line"
    tm_bad=$((tm_bad+1))
  done < <(awk '
    skip>0 { n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m; if (depth<=0) skip=0; next }
    /impl[ \t].*Measure.*[ \t]for[ \t]/ { skip=1; depth=gsub(/\{/,"{")-gsub(/\}/,"}"); if (depth<=0) skip=0; next }
    { print NR":"$0 }' "$f" | grep -E 'crate::text::(text_width|elide|cap_h)\(' | grep -vE '^[0-9]+:[[:space:]]*//' || true)
done < <(find "$SRC" -name '*.rs' | sort)
if [ "$tm_bad" -eq 0 ]; then ok "textmeasure"; else fail "textmeasure: $tm_bad line(s) outside the Measure seam"; fi

# dt (phase 12, D4 — ZERO now, was an allowlist): idle::dt() (deleted from ui/idle.rs entirely —
# card_row.rs's focused-title marquee, its last caller, now reads the absolute idle::now_ms()
# instead and wrapping_sub's two readings, motion::Phase's own drift-free idiom) and (+=|-=) *dt
# outside ui/motion.rs, which owns the integrators spec §4.2 requires for hashed logical state.
# Every clock-driven animator (spinners, ramps, hero auto-advance, the modal dip, the route
# cross-fade, the poster-preview settle) now advances through motion::Ramp/motion::Phase, which
# read Tick.ms directly and report Motion from inside advance() — the frozen-animator regression
# class this whole gate exists to catch. motion.rs currently has no hit either; the exclusion is
# the documented intent (spec §4.2: the ONLY file licensed to touch a raw per-frame delta), not a
# live carve-out.
dt_hits=$(grep_code 'idle::dt\(\)|(\+=|-=)\s*dt\b' "$SRC" | grep -v "^$SRC/ui/motion.rs:")
if [ -z "$dt_hits" ]; then ok "dt"; else
  echo "$dt_hits" | sed 's/^/    /'
  fail "dt: $(echo "$dt_hits" | wc -l | tr -d ' ') line(s) — see rule comment above"
fi

# ladder: the old per-screen focus/hit ladder shape, zero across ui/ + screens/ once every Screen
# answers FocusSource::Engine/HitSource::Engine (D2). No allowlist: every remaining hit is a
# screen or component this phase was supposed to finish converting.
gate_zero() {
  local rule="$1" pat="$2"; shift 2
  local hits; hits=$(grep_code "$pat" "$@")
  if [ -z "$hits" ]; then ok "$rule"; else
    echo "$hits" | sed 's/^/    /'
    fail "$rule: $(echo "$hits" | wc -l | tr -d ' ') line(s) — see rule comment above"
  fi
}
gate_zero ladder 'fn move_focus|fn pointer_focus|fn top_focus|fn zones\b|fn key\(sym|fn focus_is_card|fn focus_is_ctl' "$SRC/ui" "$SRC/screens"

# hittest: the narrowed raw hit-tester call shape, zero in app/ once the player's HUD registers
# its stops through DrawFrame::stop (D2) instead of app/run.rs testing raw coordinates against
# player_hud's geometry by hand. Deliberately NOT a blanket `_at(` — that false-positives on
# resume_at/memory_at/open_settings_at/write_at/profile_chip_at, which are unrelated lookups.
gate_zero hittest 'pointer_focus\(|\b(failure_quality|icon|scrub)_hit\(' "$SRC/app"

# frame: the three privileged OS-primitive calls, ZERO-TOLERANCE outside `app/run.rs` (D4) — no
# allowlist file, because there is exactly one legitimate home once D1 lands: `app/run.rs` is the
# frame loop, and `app/bridge.rs`'s `Rig` impl delegates to `run::rig_opaque_route`/
# `rig_clear_opaque_region` (a one-line pass-through) rather than naming `crate::system::` itself,
# which is what keeps this gate's text out of bridge.rs without splitting the `impl Rig<AppHost>
# for Bridge` block (a trait's impl for a type is one syntactic unit; it carries two dozen other
# methods beside these three). `ui/idle.rs`'s self-test spells two of the three call shapes as
# STRING LITERALS — it reads app/run.rs's own source text at runtime and compares against a copy
# of the exact line it expects, which is data, not a call — so a hit inside a `"…"` literal is
# stripped before matching (the same double-quote-depth tracking `tmppath` below uses), rather
# than exempting the file by name: an actual call typed into idle.rs, outside a string, still
# fails this gate. `// `-prefixed comment lines (`app/run.rs` keeps one, describing where a call
# used to live) are stripped the same way `grep_code` above does for every other rule.
frame_pat='crate::system::(ls2_pump|opaque_route|clear_opaque_region)\('
frame_bad=0
while IFS= read -r f; do
  [ "$f" = "$SRC/app/run.rs" ] && continue
  hits=$(strip_strings_and_comments "$f" | grep -nE "$frame_pat" || true)
  [ -z "$hits" ] && continue
  while IFS= read -r h; do
    ln="${h%%:*}"
    orig=$(sed -n "${ln}p" "$f")
    echo "    $f:$ln:$orig"
    frame_bad=$((frame_bad+1))
  done <<< "$hits"
done < <(find "$SRC" -name '*.rs' | sort)
if [ "$frame_bad" -eq 0 ]; then ok "frame"
else fail "frame: $frame_bad line(s) of a privileged OS-primitive call outside app/run.rs"; fi

# route: `Route::` in app/ = 0, and `enum Route` gone from the whole tree (D1/D4). No allowlist:
# the type is meant to be retired, not narrowed.
route_app=$(grep_code 'Route::' "$SRC/app")
if [ -z "$route_app" ]; then ok "route: Route:: in app/"
else fail "route: $(echo "$route_app" | wc -l | tr -d ' ') \`Route::\` use(s) in app/ — the page alphabet is \`AppArg\` (D1)"; fi
route_enum=$(grep -rn --include='*.rs' 'enum Route\b' "$SRC" 2>/dev/null || true)
if [ -z "$route_enum" ]; then ok "route: enum Route absent"
else
  echo "$route_enum" | sed 's/^/    /'
  fail "route: enum Route re-declared — the page alphabet is \`AppArg\` (D1)"
fi

# fnlen: app/run.rs::run <= 200 lines, plex_run (app/mod.rs) <= 10 lines (D4) — counted by brace
# depth from the `fn` line to its matching close, not by grep pattern.
fn_body_lines() {
  # fn_body_lines <file> <fn-name-pattern> — prints the line count of the first matching fn's body
  local file="$1" namepat="$2"
  awk -v pat="$namepat" '
    started==0 && $0 ~ ("fn " pat "\\(") { started=1; start=NR }
    started==1 {
      n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m
      if (depth<=0 && NR>start) { print NR-start+1; exit }
    }
  ' "$file"
}
run_len=$(fn_body_lines "$SRC/app/run.rs" 'run')
if [ -n "$run_len" ] && [ "$run_len" -le 200 ]; then ok "fnlen: app/run.rs::run ($run_len lines)"
else fail "fnlen: app/run.rs::run is ${run_len:-unknown} lines, budget 200 — a phase of the frame belongs in its own function (see run.rs's own doc)"; fi
plexrun_len=$(fn_body_lines "$SRC/app/mod.rs" 'plex_run')
if [ -n "$plexrun_len" ] && [ "$plexrun_len" -le 10 ]; then ok "fnlen: plex_run ($plexrun_len lines)"
else fail "fnlen: plex_run is ${plexrun_len:-unknown} lines, budget 10"; fi

# testmod: `#[cfg(test)] mod` count in app/mod.rs = 0 (D4/D8) — every test module named in D8's
# table is meant to have moved to its subject's own file by the time this gate is added.
testmod_n=$(grep -c '^#\[cfg(test)\]$' "$SRC/app/mod.rs" 2>/dev/null || echo 0)
# only count ones immediately followed by `mod `, matching the mutators gate's own convention
testmod_n=$(awk '/^#\[cfg\(test\)\]$/{p=1;next} p && /^mod /{c++} {p=0} END{print c+0}' "$SRC/app/mod.rs")
if [ "$testmod_n" -eq 0 ]; then ok "testmod: app/mod.rs"
else fail "testmod: $testmod_n \`#[cfg(test)] mod\` block(s) in app/mod.rs — a test lives beside its subject (D8)"; fi

# threads: `thread::spawn(` outside task.rs, PRODUCTION lines only, EVERY spelling — a
# `#[cfg(test)] mod` block is skipped by brace depth, the same convention the `mutators` gate
# above uses, because every remaining call site in the tree (43, re-verified 2026-09-11 after
# widening the match below) is a test's own mock TCP/HTTP server standing in for a peer, never a
# real worker (see ci/allow/threads.txt's own header). Three spellings, not one:
#   - `std::thread::spawn(`, the fully-qualified form the old grep matched;
#   - a bare `thread::spawn(` after `use std::thread;` (or `use std::thread as thread;`) — the
#     shorter spelling is a SUBSTRING of the longer one, so one `\bthread::spawn\(` pattern
#     catches both without a second pass;
#   - a bare `spawn(` after a `use std::thread::spawn;` (or a `use std::thread::{.., spawn, ..};`)
#     import, which is a different call SHAPE (`spawn(` alone) and can only be told apart from
#     every other `spawn(` in the tree — `task::spawn(`, `Builder::spawn(`, a test helper's own
#     `fn spawn(..)` — by first checking whether the file imported `spawn` from `std::thread` that
#     way. No file does today (verified 2026-09-11: zero `use std::thread::spawn` /
#     `use std::thread::{..spawn..}` lines outside task.rs itself), so this spelling adds nothing
#     to the count yet — it exists so a FUTURE import cannot go unmatched the way the bare
#     `thread::spawn(` spelling used to.
threads_bad=0
while IFS= read -r f; do
  pat='\bthread::spawn\('
  if grep -qE '^\s*use\s+std::thread::(spawn\s*;|\{[^}]*\bspawn\b[^}]*\}\s*;)' "$f"; then
    pat='\bthread::spawn\(|\bspawn\('
  fi
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    if ! allowed threads "$f"; then echo "    $f:$line"; threads_bad=$((threads_bad+1)); fi
  done < <(awk '
    skip>0 { n=gsub(/\{/,"{"); m=gsub(/\}/,"}"); depth+=n-m; if (depth<=0) skip=0; prev=$0; next }
    prev ~ /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ && /^[[:space:]]*mod / { skip=1; depth=gsub(/\{/,"{")-gsub(/\}/,"}"); if (depth<=0) skip=0; prev=$0; next }
    { print NR":"$0; prev=$0 }' "$f" | grep -E "$pat" | grep -vE '^[0-9]+:\s*//' || true)
done < <(find "$SRC" -name '*.rs' ! -path "$SRC/task.rs" | sort)
threads_declared=$(sed -n 's/^# count: *//p' ci/allow/threads.txt | head -1)
if [ "$threads_bad" -eq "${threads_declared:-0}" ]; then ok "threads"
else fail "threads: $threads_bad line(s) outside ci/allow/threads.txt (declared count is exactly ${threads_declared:-0}, not a ceiling)"; fi

# tmppath: a literal `/tmp/plxnative-` string outside dev.rs and the log sinks, = 0 (D4). The
# earlier version matched only a literal and a filesystem-open VERB co-occurring on the SAME
# LINE, which passed a two-line split (`let p = format!("/tmp/plxnative-x"); File::open(p)`)
# straight through. Rewritten as the D4 wording actually reads: strip comments, then find the
# literal, then walk the CALL that encloses it — the same paren-depth-outside-literals shape
# `tmppath`'s own header used to point at `ci/check-scrub` for; that script does not exist
# anywhere in this tree (checked 2026-09-11 — the closest real analogue is `diag/scrub.rs`'s
# redaction pass, which is Rust, not a shell gate), so the walk below is a from-scratch small
# tokenizer rather than a shared helper. It: (1) strips `//`/`///` comments to end of line, never
# treating a `//` inside a string as one; (2) tracks whether each character is inside a `"…"`
# string literal, honouring `\"` so an escaped quote does not end it early; (3) as it goes,
# maintains a stack of the CALL NAME behind every currently-open, not-yet-closed `(` (the token
# immediately before it) — which is what lets a match inside `crate::log(&format!("…"))` see BOTH
# enclosing calls, `format!` innermost and `crate::log` beneath it, across as many lines as the
# call spans. A hit is a `/tmp/plxnative-` match that is NOT inside a string, or is inside one but
# no enclosing call on that stack is `log`/`crate::log`/`log!` — i.e. exactly the two exemptions
# D4 names, comment and log-message text, and nothing else (a bare `let s = "/tmp/plxnative-x";`
# with no log() around it is a hit, deliberately, even though it opens nothing — the spec's own
# wording is "any literal…unless", not "any literal that is also an open"). `dev.rs` is the one
# structural exemption; a second category ("the log sinks") is named in the spec but resolves to
# NOTHING in this tree today — `lib.rs::events_log`/`app/boot.rs`'s crash-log open both build the
# path through `paths::in_runtime_dir("plxnative-…")`, a bare filename with no `/tmp/` prefix, so
# neither one is a `/tmp/plxnative-` literal in the first place and there is no second file to
# name here (re-verify this if a log sink is ever given a hardcoded `/tmp/` path).
tmp_hits=$(python3 - "$SRC" <<'PY'
import os, sys

src = sys.argv[1]
exempt_files = {os.path.join(src, "dev.rs")}
needle = "/tmp/plxnative-"
log_names = {"log", "crate::log", "log!"}

def scan(path, text):
    hits = []
    stack = []          # call name behind each currently-open '('
    in_str = False
    i = 0
    n = len(text)
    line = 1
    while i < n:
        c = text[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if in_str:
            if text.startswith(needle, i):
                exempt = any(name in log_names for name in stack)
                if not exempt:
                    hits.append(line)
            if c == "\\":
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        # not in a string
        if c == '"':
            in_str = True
            i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            nl = text.find("\n", i)
            i = n if nl == -1 else nl
            continue
        if c == "(":
            j = i - 1
            while j >= 0 and text[j] in " \t\n":
                j -= 1
            k = j
            while k >= 0 and (text[k].isalnum() or text[k] in "_:!"):
                k -= 1
            stack.append(text[k + 1:j + 1])
            i += 1
            continue
        if c == ")":
            if stack:
                stack.pop()
            i += 1
            continue
        if text.startswith(needle, i):
            # a literal outside any string at all — not valid Rust for a real path, but still
            # not comment or log-message text, so it counts.
            hits.append(line)
        i += 1
    return hits

bad = 0
for root, _dirs, files in os.walk(src):
    for fn in sorted(files):
        if not fn.endswith(".rs"):
            continue
        path = os.path.join(root, fn)
        if path in exempt_files:
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        if needle not in text:
            continue
        for ln in scan(path, text):
            src_line = text.splitlines()[ln - 1]
            print(f"{path}:{ln}:{src_line}")
            bad += 1
sys.exit(1 if bad else 0)
PY
)
tmp_status=$?
if [ "$tmp_status" -eq 0 ] && [ -z "$tmp_hits" ]; then ok "tmppath"
else
  echo "$tmp_hits" | sed 's/^/    /'
  fail "tmppath: $(echo "$tmp_hits" | grep -c . ) line(s) of a literal /tmp/plxnative- path outside dev.rs, not comment or log-message text"
fi


# every allowlist's declared count equals its entries
for f in ci/allow/*.txt; do
  declared=$(sed -n 's/^# count: *//p' "$f" | head -1)
  entries=$(grep -cvE '^\s*(#|$)' "$f")
  [ "$declared" = "$entries" ] || fail "$f declares count $declared but has $entries entries"
done

[ "$fails" -eq 0 ] && { echo "check-deps: all gates green"; exit 0; }
echo "check-deps: $fails gate(s) failed"; exit 1
