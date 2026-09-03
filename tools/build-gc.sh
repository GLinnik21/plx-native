#!/bin/sh
# Report and reclaim this repository's DERIVED build trees, across every worktree at once.
#
# WHY THIS EXISTS. The build products of this project are large, per-checkout and unbounded, and
# the fleet workflow (`.agents/skills/fleet-plan`) multiplies them by the number of lanes. Nothing
# ever collected them, so the failure mode was not a warning — it was a volume at 99% with 3.2 GiB
# free, measured 2026-09-03 across twelve lanes plus the main checkout:
#
#   45 GB   the repo tree
#   24 GB   ⤷ `target*/debug/incremental` — a CACHE OF THE LAST BUILD, 1.2-4.0 GB per lane
#   14 GB   ⤷ `target*/debug/deps` and the cross-build outputs
#    2.6 GB ⤷ vendor/ffmpeg-build — twelve byte-identical copies of one 122 MB object tree
#
# ...and then a further **36 GB that none of those numbers could see**, because `fleet-plan` tells
# workers to point `CARGO_TARGET_DIR` at `$HOME/plx-fleet/<lane>` — OUTSIDE the repository, so
# outside every `du` anyone had run. Ten lanes' worth, every one of them an ORPHAN whose worktree
# had long since been removed. That is what `--orphans` is for, and it is the mode to reach for
# first: nothing on the machine will ever refer to those trees again.
#
# Read that top-to-bottom before reaching for the thing everyone reaches for. **FFmpeg is 6% of
# it.** The compile cache is 53%. Two changes since address the standing halves — `CARGO_INCREMENTAL=0`
# in a linked worktree (the Makefile, beside `RUST_FEATFLAGS`) and a shared flag-keyed FFmpeg
# build tree (`ci/build-ffmpeg.sh`) — and this script is the third: the one that reclaims what is
# already on disk, and the one to run when a build starts failing for space.
#
# WHAT IS SAFE. Everything this script deletes is reproducible by `make` from tracked sources, and
# it never touches a file git knows about. It deletes only paths matching the derived-tree names
# below, and it will not touch the MAIN checkout's `target/` unless asked (that is the tree a human
# iterates in; a lane's is cut for one task).
set -eu

# BEFORE THE `cd`, because `ci/build-ffmpeg.sh` resolves a relative PLX_BUILD_CACHE against the
# CALLER's directory and this script is about to change to the repository root. Left alone, the two
# would resolve `PLX_BUILD_CACHE=.plx-cache` to different places: the report would describe a cache
# that does not exist, the prune would target the wrong tree, and — worst of the three — the busy
# guard would look for active locks in a directory no build is using and cheerfully find none.
if [ -n "${PLX_BUILD_CACHE-}" ] && [ -d "${PLX_BUILD_CACHE-}" ]; then
  PLX_BUILD_CACHE=$(cd "$PLX_BUILD_CACHE" && pwd)
  export PLX_BUILD_CACHE
fi
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
MAIN=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null | sed 's|/\.git$||')
[ -n "$MAIN" ] || MAIN="$ROOT"

MODE=report
DRY=
for a in "$@"; do
  case "$a" in
    --report)      MODE=report ;;
    --incremental) MODE=incremental ;;
    --lanes)       MODE=lanes ;;
    --orphans)     MODE=orphans ;;
    --cache)       MODE=cache ;;
    --all)         MODE=all ;;
    -n|--dry-run)  DRY=1 ;;
    -h|--help)
      sed -n '2,/^set -eu/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'
      cat <<'USAGE'

usage: tools/build-gc.sh [MODE] [-n]

  (no mode)       report every checkout's derived trees, the shared cache and free space
  --incremental   delete `target*/debug/incremental` everywhere. Always safe: it is a compile
                  cache, and a linked worktree does not even write one any more.
  --lanes         delete every derived tree in the LINKED WORKTREES — cargo target dirs and the
                  vendor build trees — plus the EXTERNAL lane target dirs under $PLX_FLEET_DIR
                  (default ~/plx-fleet), which is where fleet-plan tells workers to point
                  CARGO_TARGET_DIR and which outlive their worktree. The main checkout is left
                  alone. Costs each lane one rebuild when it next runs; costs no source,
                  committed or not.
  --orphans       delete ONLY the external lane target dirs whose worktree no longer exists.
                  The narrowest mode and the one to reach for first: nothing on this machine
                  will ever refer to those trees again, and no live lane pays a rebuild.
  --cache         delete shared FFmpeg build trees under $PLX_BUILD_CACHE untouched for
                  $PLX_CACHE_MAX_DAYS days (default 30). They are keyed by configure flags AND
                  toolchain, so a version bump or an NDK upgrade strands the old entry silently —
                  a cache nothing prunes is the same unbounded growth this script exists for.
                  The tarball is kept; it is 11 MB and every checkout copies from it.
  --all           --incremental everywhere plus --lanes, --orphans and --cache, and the main checkout's vendor build
                  trees. Leaves the main checkout's target dirs.
  -n, --dry-run   print what would go, delete nothing.

Nothing here touches a tracked file, the shared FFmpeg cache under $PLX_BUILD_CACHE, or the
television. Everything it removes is rebuilt by `make`.
USAGE
      exit 0 ;;
    *) echo "build-gc: unknown argument $a (try --help)" >&2; exit 2 ;;
  esac
done

# A delete under a live `cargo` is how a target dir becomes corrupt rather than absent, and this
# script cannot tell which checkout a running rustc belongs to. Refusing globally is the honest
# reading of what it can see.
# ENUMERATE ONCE, AND FAIL CLOSED. The old form ran `git worktree list` inside a pipeline, so its
# exit status was masked by the `sed` that followed and a git failure came back as an EMPTY,
# SUCCESSFUL list — whereupon `external_is_orphan` would find no live lane matching anything and
# `--orphans` would delete every fleet target dir on the machine, including the ones being built
# in. An empty answer to "which worktrees exist" is never a licence to delete; in this repository
# it cannot even be true, since the checkout asking the question is itself one.
WT_RAW=$(git worktree list --porcelain 2>/dev/null) || WT_RAW=""
worktrees() {
  printf '%s\n' "$WT_RAW" | sed -n 's/^worktree //p' | while IFS= read -r w; do
    [ -d "$w" ] && echo "$w"
  done
}

# Sizes are summed in KB and formatted once at the end. `du -sh` cannot be added up, and `du -sch`
# over a list cannot be given an EMPTY list — with no arguments it measures the current directory,
# which is how the first version of this table reported 193M of cargo output in three checkouts
# that had none.
sum_kb() {
  total=0
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    kb=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
    total=$((total + ${kb:-0}))
  done
  echo "$total"
}
fmt_kb() {
  awk -v k="${1:-0}" 'BEGIN{
    if (k <= 0)          printf "0B";
    else if (k >= 1048576) printf "%.1fG", k/1048576;
    else if (k >= 1024)    printf "%.0fM", k/1024;
    else                   printf "%dK", k }'
}

# Registered worktrees living INSIDE another checkout. This repo's own layout puts every lane at
# `$MAIN/.claude/worktrees/<lane>`, so a plain `du -sh "$MAIN"` bills the main row for every lane
# as well — and each of those lanes then gets its own row underneath, counted twice. The fleet
# skill already names this exact trap for `du -sh .` at the repo root; a tool that reports the
# number has no excuse for reproducing it.
nested_of() {
  parent=$1
  worktrees | while IFS= read -r o; do
    [ "$o" = "$parent" ] && continue
    case "$o" in "$parent"/*) echo "$o" ;; esac
  done
}
checkout_kb() {
  t=$(du -sk "$1" 2>/dev/null | awk '{print $1}')
  n=$(nested_of "$1" | sum_kb)
  echo $(( ${t:-0} - n ))
}

# The derived-tree names, in one place. `rust-modules/target*` covers every feature set's dir
# (target, -release, -sim, -lab, -sym, -macapp, -shots); the vendor entries are the FFmpeg and
# Sentry source and object trees, whose only product is the small prefix beside them.
lane_trees() {
  for d in "$1"/rust-modules/target*; do
    case "$d" in *'*'*) continue ;; esac
    [ -d "$d" ] && echo "$d"
  done
  vendor_trees "$1"
}
# The vendor half on its own, because TWO callers need exactly this list and the second one used
# to spell out a shorter version by hand — `--all` swept the main checkout's FFmpeg trees and left
# its Sentry source and build trees behind, while claiming in its own heading to have taken "the
# main checkout's vendor build trees". One definition cannot drift from itself.
vendor_trees() {
  for d in "$1"/vendor/ffmpeg-build/ffmpeg-* "$1"/vendor/ffmpeg-build-host/ffmpeg-* \
           "$1"/vendor/ffmpeg-build/destdir "$1"/vendor/ffmpeg-build-host/destdir \
           "$1"/vendor/sentry-native-build "$1"/vendor/sentry-native-src; do
    case "$d" in *'*'*) continue ;; esac
    [ -d "$d" ] && echo "$d"
  done
}
incremental_trees() {
  for d in "$1"/rust-modules/target*/debug/incremental; do
    case "$d" in *'*'*) continue ;; esac
    [ -d "$d" ] && echo "$d"
  done
}

# LANE BUILD TREES THAT ARE NOT IN A LANE. `fleet-plan` tells every worker to export
# `CARGO_TARGET_DIR=$HOME/plx-fleet/<lane>/target` and `SIM_TDIR=.../target-sim`, precisely so that
# `git worktree remove` stays meaningful — which means the documented default puts the biggest
# thing this script exists to find OUTSIDE every path it was walking. Worse, those trees outlive
# the worktree by construction: remove the lane and its gigabytes stay, owned by nobody and named
# after a branch that no longer exists. Scanning only `target*` under each lane directory keeps
# this to cargo output; nothing else in there is touched.
FLEET_DIR=${PLX_FLEET_DIR-$HOME/plx-fleet}
external_trees() {
  [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ] || return 0
  for d in "$FLEET_DIR"/*/target*; do
    case "$d" in *'*'*) continue ;; esac
    [ -d "$d" ] && echo "$d"
  done
}
external_incremental_trees() {
  [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ] || return 0
  for d in "$FLEET_DIR"/*/target*/debug/incremental; do
    case "$d" in *'*'*) continue ;; esac
    [ -d "$d" ] && echo "$d"
  done
}

# AN EXPORTED `CARGO_TARGET_DIR` IS REPORTED AND NEVER DELETED, and the asymmetry is the whole
# point. A path from the environment is arbitrary: it can be a directory shared by several
# checkouts, it can be somewhere entirely unrelated to this project, and its PARENT's basename —
# which is how an external tree is matched back to a lane — means nothing at all, so the orphan
# test would classify it from a coincidence. Deleting it would be exactly the "preserve unrelated
# generated artifacts" rule this repository states outright. Every destructive mode is therefore
# scoped to $FLEET_DIR, whose layout this script defines; anything else is shown so you can decide
# for yourself.
env_trees_report_only() {
  for d in ${CARGO_TARGET_DIR:+"$CARGO_TARGET_DIR"} ${SIM_TDIR:+"$SIM_TDIR"}; do
    [ -d "$d" ] || continue
    case "$d" in "$FLEET_DIR"/*) continue ;; esac   # already covered, and covered deletably
    echo "$d"
  done
}
# Is this external tree's lane still a registered worktree? A directory under $FLEET_DIR is named
# for its lane, so the answer is a basename match — and a `no` is the interesting case, because
# nothing else on the machine will ever mention it again.
external_is_orphan() {
  lane=$(basename "$(dirname "$1")")
  worktrees | while IFS= read -r w; do
    [ "$(basename "$w")" = "$lane" ] && echo live
  done | grep -q live && return 1
  return 0
}

# Delete $1 while reporting the name the user knows it by, which is the one that was on stdin.
drop_named() {
  target=$1
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    sz=$(fmt_kb "$(du -sk "$target" 2>/dev/null | awk '{print $1}')")
    printf '  removed %8s  %s\n' "$sz" "$d"
    rm -rf "$target"
  done
}
drop() {
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    sz=$(fmt_kb "$(du -sk "$d" 2>/dev/null | awk '{print $1}')")
    if [ -n "$DRY" ]; then printf '  would remove %8s  %s\n' "$sz" "$d"
    else printf '  removed %8s  %s\n' "$sz" "$d"; rm -rf "$d"; fi
  done
}

# THE PREFLIGHT GOES HERE, ABOVE EVERY DISPATCH. It used to sit further down, which put it after
# the `--incremental` case — so that mode deleted its trees and only then asked whether a build was
# running, in a script whose refusal message promises the opposite. The check is worth nothing
# unless it precedes the first thing that can delete.
# WHAT COUNTS AS "A BUILD IS RUNNING" IS WIDER THAN CARGO. Most of a `make` here is not cargo at
# all — FFmpeg's own configure and make, the Sentry Native CMake build, the C translation units,
# the final link — and during every one of those phases neither `cargo` nor `rustc` exists as a
# process. A guard that watched only those two would pass happily and delete the very tree the
# build was reading, which is not a clean failure: it is a half-removed object tree and a build
# that fails somewhere unrelated. So the list covers the cross compiler and `make` as well, and
# the authoritative signal for the shared FFmpeg tree is its own LOCK — if one is held by a live
# process, somebody is inside `ci/build-ffmpeg.sh` right now.
#
# A dry run deletes nothing, so it is exempt — and it has to be, because "is a build running?" is
# precisely the question you are asking when you reach for `-n` in the middle of a fleet.
# `pgrep -x` matches the process NAME exactly. Deliberately not `pgrep -f`, which matches whole
# command lines and would find this script's own invocation the moment anything on it mentioned
# cargo — the self-match trap that has already made a finished job here read as still running.
build_is_running() {
  for n in cargo rustc make cc1 arm-webos-linux-gnueabi-gcc; do
    if pgrep -x "$n" >/dev/null 2>&1; then echo "a running $n"; return 0; fi
  done
  c=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
  if [ -n "$c" ]; then
    for l in "$c"/ffmpeg/*.lock; do
      case "$l" in *'*'*) continue ;; esac
      [ -d "$l" ] || continue
      if owner_is_alive "$l"; then
        echo "a held FFmpeg cache lock ($l)"; return 0
      fi
    done
  fi
  return 1
}
if [ "$MODE" != report ] && [ -z "$DRY" ]; then
  if [ -z "$(worktrees)" ]; then
    echo "build-gc: cannot enumerate this repository's worktrees — refusing to delete anything." >&2
    echo "          Every reclaim mode decides what is dead from that list, so an empty one is a" >&2
    echo "          reason to stop, not a licence. Check that git works here and retry." >&2
    exit 1
  fi
  if busy=$(build_is_running); then
    echo "build-gc: $busy — refusing to delete a build tree underneath it." >&2
    echo "          Wait for it, or re-run when the fleet is idle. (-n previews regardless.)" >&2
    exit 1
  fi
fi

# `git worktree list --porcelain` emits `worktree <path>`, and the path may contain SPACES —
# `awk '{print $2}'` truncates it at the first one, so the lane is silently missed by the report
# and, worse, `--lanes` would go looking under a prefix that is somebody else's directory. Strip
# the fixed prefix instead and consume the list with `while IFS= read -r`, never a bare `$(...)`
# in a `for`, which re-splits on whitespace one line later.
# ...and a path git still lists but which is NOT THERE is not a live worktree. Delete a lane's
# directory without `git worktree remove` and git keeps a `prunable` entry for it — which the
# orphan test would read as "the lane exists", leaving that lane's external target dir, the most
# abandoned output on the machine, as the one thing `--orphans` refuses to collect.


case "$MODE" in
report)
  printf '%-46s %8s %8s %8s  %s\n' CHECKOUT TOTAL TARGETS INCR BRANCH
  worktrees | while IFS= read -r w; do
    printf '%-46s %8s %8s %8s  %s\n' "$(basename "$w")" \
           "$(fmt_kb "$(checkout_kb "$w")")" \
           "$(fmt_kb "$(lane_trees "$w" | sum_kb)")" \
           "$(fmt_kb "$(incremental_trees "$w" | sum_kb)")" \
           "$(git -C "$w" rev-parse --abbrev-ref HEAD 2>/dev/null)"
  done
  echo "  (a checkout nested inside another is subtracted from that one's TOTAL, not counted twice)"

  ext=$(external_trees | sort -u)
  if [ -n "$ext" ]; then
    echo
    echo "external lane build trees ($FLEET_DIR):"
    printf '%s\n' "$ext" | while IFS= read -r d; do
      if external_is_orphan "$d"; then tag='  ORPHAN — its worktree is gone'; else tag=''; fi
      printf '  %8s  %s%s\n' "$(fmt_kb "$(du -sk "$d" 2>/dev/null | awk '{print $1}')")" "$d" "$tag"
    done
  fi
  envx=$(env_trees_report_only | sort -u)
  if [ -n "$envx" ]; then
    echo
    echo "exported CARGO_TARGET_DIR / SIM_TDIR (reported only — this script never deletes these):"
    printf '%s\n' "$envx" | while IFS= read -r d; do
      printf '  %8s  %s\n' "$(fmt_kb "$(du -sk "$d" 2>/dev/null | awk '{print $1}')")" "$d"
    done
  fi

  cache=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
  echo
  if [ -n "$cache" ] && [ -d "$cache" ]; then
    printf 'shared build cache  %8s  %s (one copy per configuration, for every checkout)\n' \
           "$(fmt_kb "$(du -sk "$cache" 2>/dev/null | awk '{print $1}')")" "$cache"
  fi
  df -h "$ROOT" | tail -1 | awk '{print "volume              " $4 " free of " $2}'
  echo
  echo "reclaim with: tools/build-gc.sh --orphans | --incremental | --cache | --lanes | --all   (add -n to preview)"
  ;;
esac

case "$MODE" in
incremental|all)
  echo "== cargo incremental caches (a compile cache; rebuilt on demand) =="
  # Including the ones under $FLEET_DIR. A lane that followed fleet-plan put its target dir
  # outside the worktree, so its `debug/incremental` is outside `$w/rust-modules/target*` too —
  # and the incremental cache is the single largest thing this script exists to reclaim, so a
  # mode advertised as clearing it cannot be blind to where a fleet actually keeps it.
  { worktrees | while IFS= read -r w; do incremental_trees "$w"; done
    external_incremental_trees; } | sort -u | drop
  ;;
esac

CACHE_MAX_DAYS=${PLX_CACHE_MAX_DAYS-30}
# Take a cache entry's lock the same way `ci/build-ffmpeg.sh` does, INCLUDING its reclaim rule —
# otherwise the one thing guaranteed to leave a dead lock behind (a build that was killed) is also
# the thing that makes its tree permanently unprunable, so the entries most worth collecting are
# exactly the ones that never are. `mv` is the atomic claim; a lock under a minute old is left
# alone, because that is the window in which a live owner has not yet written its pid.
# THE SAME LIVENESS RULE AS THE BUILDER, both halves of it. This checked only the pid, while
# `ci/build-ffmpeg.sh` also checks the recorded PROCESS GROUP — so a build shell killed while its
# `configure` or `make` child kept writing looked dead to the prune and alive to every other
# builder, and `--cache` would take the lock and delete the tree out from under a live compiler.
# Two implementations of one rule is how that happens; this is now the same test, written once.
#
# And `kill -0 0` does not mean "dead": POSIX reads pid 0 as the caller's process group, so it
# SUCCEEDS. With the old `|| echo 0` fallback a lock with no pid file read as permanently alive.
owner_is_alive() {   # $1 = lock directory
  _pid=$(cat "$1/pid" 2>/dev/null || echo 0)
  _pgid=$(cat "$1/pgid" 2>/dev/null || echo 0)
  case "$_pid"  in ''|*[!0-9]*) _pid=0  ;; esac
  case "$_pgid" in ''|*[!0-9]*) _pgid=0 ;; esac
  [ "$_pid"  -gt 0 ] && kill -0 "$_pid" 2>/dev/null && return 0
  [ "$_pgid" -gt 0 ] && pgrep -g "$_pgid" >/dev/null 2>&1 && return 0
  return 1
}
prune_lock() {
  l="$1.lock"
  if mkdir "$l" 2>/dev/null; then echo $$ > "$l/pid"; ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' > "$l/pgid" || true; return 0; fi
  if ! owner_is_alive "$l" && [ -n "$(find "$l" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
    if mv "$l" "$l.stale.$$" 2>/dev/null; then rm -rf "$l.stale.$$"; fi
    if mkdir "$l" 2>/dev/null; then echo $$ > "$l/pid"; ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' > "$l/pgid" || true; return 0; fi
  fi
  return 1
}
stale_cache_trees() {
  c=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
  [ -n "$c" ] && [ -d "$c/ffmpeg" ] || return 0
  for d in "$c"/ffmpeg/*; do
    case "$d" in
      *'*'*)   continue ;;
      # A LOCK IS NOT A CACHE ENTRY. A dead `<key>.lock` (or a `<key>.lock.stale.<pid>` left by an
      # interrupted reclaim) ages past the threshold like anything else, and emitting it here was
      # doubly wrong: the prune would try to protect it with `<key>.lock.lock` and could `rm -rf` a
      # lock a build had just reacquired — destroying the mutex it was meant to respect — while the
      # real work tree beside it was SKIPPED, because its own lock still existed. Filtered here;
      # dead locks are reclaimed by `prune_lock` below, which is the code that understands them.
      *.lock|*.lock.*) continue ;;
      # A tombstone is a delete somebody interrupted; it matches no cache key, so nothing will
      # ever read it. Sweep it and move on rather than reporting it as an entry — but NOT under
      # `-n`, which promises to mutate nothing and was quietly doing an `rm -rf` here on its way
      # past. A preview that deletes is worse than no preview.
      *.tombstone.*) if [ -z "$DRY" ]; then rm -rf "$d"; fi; continue ;;
    esac
    [ -d "$d" ] || continue
    # Age the LAST-USED MARKER that build-ffmpeg.sh touches on every successful run. A directory's
    # own mtime moves only when its direct children change, so the busiest configuration on the
    # machine — rebuilt into and copied out of daily, all of it below `ffmpeg-9.0/` — would keep
    # the mtime it was created with and be collected on day thirty. An entry with no marker at all
    # predates that mechanism, and falls back to the directory.
    stamp="$d/.last-used"; [ -f "$stamp" ] || stamp="$d"
    if [ -n "$(find "$stamp" -maxdepth 0 -mtime +"$CACHE_MAX_DAYS" 2>/dev/null)" ]; then echo "$d"; fi
  done
}

case "$MODE" in
cache|all)
  echo "== shared build trees untouched for over $CACHE_MAX_DAYS days =="
  # UNDER EACH ENTRY'S OWN LOCK. The preflight guard is a single check at startup, so a build that
  # starts a moment later takes `$WORK.lock` and begins reading a tree this loop is already
  # committed to deleting — and the whole reason that lock exists is that the tree is shared.
  # Taking it here makes the prune one more participant in the same protocol rather than an
  # exception to it: if the lock cannot be had, the entry is in use and is simply skipped, which
  # is the right answer for something being collected only because it looked idle for a month.
  stale_cache_trees | while IFS= read -r d; do
    [ -n "$d" ] || continue
    # A DRY RUN TAKES NO LOCK. Only `drop` consulted $DRY, so a preview was creating lock
    # directories, reclaiming stale ones and removing its own again — mutating the very protocol
    # it was previewing, and leaving real builds waiting on its lock if it was interrupted. `-n`
    # is advertised as deleting nothing; it must also mean touching nothing.
    if [ -n "$DRY" ]; then
      echo "$d" | drop
    elif prune_lock "$d"; then
      # RENAME FIRST, DELETE THE TOMBSTONE AFTER. `rm -rf` on a 122 MB tree is not instantaneous,
      # and interrupting it leaves `$WORK/ffmpeg-9.0` PRESENT AND PARTIAL — which is precisely the
      # state `ci/build-ffmpeg.sh` reads as "already extracted", so once the abandoned lock is
      # reclaimed every checkout builds against a source tree with holes in it and the
      # configuration stays broken until somebody deletes it by hand. The rename is atomic, so the
      # entry is either wholly there or wholly gone; a tombstone left by an interrupted delete
      # matches no cache key and is swept by the next run.
      if mv "$d" "$d.tombstone.$$" 2>/dev/null; then
        echo "$d" | drop_named "$d.tombstone.$$"
      else
        echo "$d" | drop
      fi
      rm -rf "$d.lock"
    else
      echo "  in use, skipped  $d"
    fi
  done
  ;;
esac

case "$MODE" in
orphans)
  echo "== external lane target dirs whose worktree is gone =="
  external_trees | sort -u | while IFS= read -r d; do
    if external_is_orphan "$d"; then echo "$d"; fi
  done | drop
  ;;
esac

case "$MODE" in
all)
  echo "== external lane target dirs whose worktree is gone =="
  external_trees | sort -u | while IFS= read -r d; do
    if external_is_orphan "$d"; then echo "$d"; fi
  done | drop
  ;;
esac

case "$MODE" in
lanes|all)
  echo "== derived trees in linked worktrees (the main checkout is left alone) =="
  worktrees | while IFS= read -r w; do
    [ "$w" = "$MAIN" ] && continue
    lane_trees "$w"
  done | drop
  echo "== external lane build trees =="
  external_trees | sort -u | drop
  ;;
esac

case "$MODE" in
all)
  echo "== the main checkout's vendor build trees (its target dirs are kept) =="
  vendor_trees "$MAIN" | drop
  ;;
esac

if [ "$MODE" != report ]; then
  echo
  df -h "$ROOT" | tail -1 | awk '{print "free now: " $4 " of " $2}'
fi
