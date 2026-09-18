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


# THE INCREMENTAL POLICY, ENFORCED WHERE IT LEAKS.
#
# The Makefile sets `CARGO_INCREMENTAL=0` in a linked worktree, with a long comment explaining why
# a lane cut for one task must not pay a multi-gigabyte cache of its last build. That rule is
# correct and it does not hold, because it lives in ONE of the several places a `cargo` runs here:
# every agent (and `AGENTS.md`'s own `--no-default-features` gate) invokes cargo directly, and a
# direct invocation never reads the Makefile. Measured 2026-09-17 across this machine's 113
# worktrees: **12.9 GB of `target*/debug/incremental` inside LINKED worktrees**, on a volume with
# 5.1 GiB free — i.e. the policy's whole stated saving, still on the disk, in the checkouts it
# names.
#
# Cargo merges `.cargo/config.toml` upward from the working directory and the NEAREST setting
# wins, so a file at the worktrees ROOT applies to every lane under it and to nothing else — the
# main checkout sits above that directory and keeps its cache, which is exactly the Makefile's
# rule, now written where any cargo can see it. An explicit `CARGO_INCREMENTAL=1` still wins: an
# environment variable outranks a config file, so the documented escape hatch for a lane doing
# genuinely long iterative work is unchanged.
#
# Installed from here rather than committed because `.claude/worktrees/` is gitignored — it is
# local scratch, and this is the one tool that already owns what lives in it. Idempotent, and
# never written if a human has put something else in that file.
install_worktree_cargo_policy() {
  wt_root="$MAIN/.claude/worktrees"
  [ -d "$wt_root" ] || return 0
  cfg="$wt_root/.cargo/config.toml"
  if [ -f "$cfg" ]; then
    # The marker carries a version because the policy itself grew one: v1 (2026-09-17) was
    # `incremental = false` alone, and v2 (2026-09-18, below) adds the debuginfo trim. A file
    # carrying the v2 marker is already current — nothing to do. A file carrying the BARE marker
    # (v1, or any future policy that forgot to version itself) is OURS but stale, and gets
    # REPLACED below rather than left — the whole reason it is a marker and not just "file
    # exists" is so this script can tell its own output from a human's. A file with no marker at
    # all is somebody else's and is never touched, on this run or any later one.
    if grep -q 'plx-build-gc-policy v2' "$cfg" 2>/dev/null; then return 0; fi
    if ! grep -q 'plx-build-gc-policy' "$cfg" 2>/dev/null; then return 0; fi
  fi
  # `-n` writes nothing, including this. A dry run that creates or rewrites a file is not a dry run.
  if [ -n "$DRY" ]; then
    if [ -f "$cfg" ]; then echo "build-gc: would update the linked-worktree cargo policy at $cfg (v1 -> v2)"
    else echo "build-gc: would install the linked-worktree cargo policy at $cfg"; fi
    return 0
  fi
  mkdir -p "$wt_root/.cargo" || return 0
  cat > "$cfg" <<'POLICY'
# plx-build-gc-policy v2 — installed by tools/build-gc.sh, not tracked by git.
#
# Cargo merges config upward from the working directory, so this file applies to every linked
# worktree under `.claude/worktrees/` and to NOTHING else: the main checkout lives above this
# directory and keeps its incremental cache and full DWARF, which is the policy the Makefile
# states beside `RUST_FEATFLAGS`. The Makefile can only enforce it for its own cargo invocations;
# an agent running `cargo test` or `cargo check` by hand bypassed it, and 12.9 GB of lane
# incremental caches is what that cost.
#
# `CARGO_INCREMENTAL=1` in the environment still wins — an env var outranks a config file — so a
# lane that really is doing long iterative work can still buy the cache back for itself.
[build]
incremental = false

# v2, 2026-09-18. The incremental cache reads 0 B in every lane now that the rule above actually
# holds — but a lane's `target/` did not shrink with it, because it was never the incremental
# cache doing most of the damage there. A measured lane's target dir was 4.0 GB, of which 3.1 GB
# was `debug/deps`, of which 1.5 GB was `*.rcgu.o` — one object file per codegen unit, left next
# to the test binary because this host's cargo defaults to `split-debuginfo=unpacked` rather than
# packing debuginfo into the binary or dropping it. The main checkout carries the identical shape
# at larger scale (4.1 GB of `.o` in a 5.5 GB `debug/`), which is precisely why this section does
# NOT apply there: a lane is cut for one task and its whole target dir is thrown away with the
# worktree (see `--worktrees` below), so it has no use for the debugger's-eye view those object
# files exist for — only the coarse line-table entry a panic backtrace needs. `line-tables-only`
# keeps that; `debug = false` on every third-party package drops the same bloat in code this
# repository does not own or step through anyway.
#
# Measured 2026-09-18 against this crate's own `--lib --no-run` test build (host target, two
# clean /tmp target dirs so nothing but the profile differed): full DWARF (`debug = 2`, cargo's
# own default) produced a 789 MB `debug/`, of which the root crate's 16 `*.rcgu.o` codegen units
# alone were 318 MB; this policy produced a 703 MB `debug/` with the same 16 files at 236 MB — an
# 11% cut to the whole tree and a 26% cut to the object files directly, from ONE crate's `--lib`
# closure. The saving is modest, plainly: most of the object-file bulk is code, not DWARF.
#
# `CARGO_PROFILE_DEV_DEBUG=2` in the environment still overrides this, the same escape hatch as
# `CARGO_INCREMENTAL=1` above: an env var outranks a config file, so a lane genuinely attaching
# `lldb`/`rust-lldb` to a host test can buy full DWARF back for itself without touching this file.
#
# Checked before landing that nothing else in the nearer config chain already sets this and would
# make the addition a no-op or a conflict: neither `rust-modules/Cargo.toml` (only
# `[profile.release]`) nor `rust-modules/.cargo/config.toml` (only `[target...]` rustflags and
# `[unstable] build-std`) mentions `[profile.dev]` debug. Cargo resolves a manifest `[profile]`
# against a config `[profile]` by letting config win, and the nearest config wins over a farther
# one — both rules favour this file over anything upstream, which is the reason it is safe to add
# here rather than in the crate manifest.
[profile.dev]
debug = "line-tables-only"
[profile.dev.package."*"]
debug = false
POLICY
  echo "build-gc: installed the linked-worktree cargo policy (v2) at $cfg"
}
MODE=report
DRY=
for a in "$@"; do
  case "$a" in
    --report)      MODE=report ;;
    --incremental) MODE=incremental ;;
    --lanes)       MODE=lanes ;;
    --orphans)     MODE=orphans ;;
    --worktrees)   MODE=worktrees ;;
    --cache)       MODE=cache ;;
    --all)         MODE=all ;;
    -n|--dry-run)  DRY=1 ;;
    -h|--help)
      sed -n '2,/^set -eu/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'
      cat <<'USAGE'

usage: tools/build-gc.sh [MODE] [-n]

  (no mode)       report every checkout's derived trees, the shared cache and free space
  --incremental   delete `target*/debug/incremental` everywhere. Always safe: it is a compile
                  cache. A linked worktree is not supposed to write one — but the Makefile's
                  `CARGO_INCREMENTAL=0` only reaches the cargo runs `make` launches, and 12.9 GB
                  of them had accumulated past it by 2026-09-17, which is why this script now
                  installs the same rule as a `.cargo/config.toml` any cargo can see.
  --lanes         delete every derived tree in the LINKED WORKTREES — cargo target dirs and the
                  vendor build trees — plus the EXTERNAL lane target dirs under $PLX_FLEET_DIR
                  (default ~/plx-fleet), which is where fleet-plan tells workers to point
                  CARGO_TARGET_DIR and which outlive their worktree. The main checkout is left
                  alone. Costs each lane one rebuild when it next runs; costs no source,
                  committed or not.
  --orphans       delete ONLY the external lane target dirs whose worktree no longer exists.
                  The narrowest mode and the one to reach for first: nothing on this machine
                  will ever refer to those trees again, and no live lane pays a rebuild.
  --worktrees     remove FINISHED linked worktrees outright: working tree clean, not locked, not
                  the main checkout, not the checkout this script is running from, and its HEAD
                  is already on `main` (`git merge-base --is-ancestor`, or a squash-merge check
                  via `git merge-tree --write-tree` against `main`'s tree — the check
                  `git branch --merged` cannot make, and the shape every lane here actually
                  lands as, per AGENTS.md's squash-only trunk rule). Removal is a plain
                  `git worktree remove` (no --force, so it refuses rather than eating anything
                  the clean-tree check missed) followed by that lane's external target dir under
                  $PLX_FLEET_DIR if one exists. Branches are never deleted; a removed worktree
                  whose branch still exists is reported as "branch left" so the owner decides.
                  Every worktree NOT removed is reported with one reason: dirty | locked |
                  unmerged | building | current | main.
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
# `if ... fi`, not `[ -d "$w" ] && echo "$w"`: under `set -e`, a loop's LAST statement failing
# (the test false, so the `&&` short-circuits with a nonzero status) becomes the exit status of
# the enclosing function or `while` subshell. A caller that then pipes this function's output
# (safe — non-last pipeline stages are exempt) is fine, but a caller that invokes it as a bare
# command, or a `while read` loop whose own last statement is a bare call to a function shaped
# like this, aborts that subshell on the FIRST iteration whose candidate happens to not exist —
# which silently truncated `--lanes`/`--incremental` to one worktree. `if/fi` always returns 0.
worktrees() {
  printf '%s\n' "$WT_RAW" | sed -n 's/^worktree //p' | while IFS= read -r w; do
    if [ -d "$w" ]; then echo "$w"; fi
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
    if [ -d "$d" ]; then echo "$d"; fi
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
    if [ -d "$d" ]; then echo "$d"; fi
  done
}
incremental_trees() {
  for d in "$1"/rust-modules/target*/debug/incremental; do
    case "$d" in *'*'*) continue ;; esac
    if [ -d "$d" ]; then echo "$d"; fi
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
    if [ -d "$d" ]; then echo "$d"; fi
  done
}
external_incremental_trees() {
  [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ] || return 0
  for d in "$FLEET_DIR"/*/target*/debug/incremental; do
    case "$d" in *'*'*) continue ;; esac
    if [ -d "$d" ]; then echo "$d"; fi
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
    if [ "$(basename "$w")" = "$lane" ]; then echo live; fi
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
# THE SAME LIVENESS RULE AS THE BUILDER, both halves of it. This checked only the pid, while
# `ci/build-ffmpeg.sh` also checks the recorded PROCESS GROUP — so a build shell killed while its
# `configure` or `make` child kept writing looked dead to the prune and alive to every other
# builder, and `--cache` would take the lock and delete the tree out from under a live compiler.
# Two implementations of one rule is how that happens; this is now the same test, written once.
#
# And `kill -0 0` does not mean "dead": POSIX reads pid 0 as the caller's process group, so it
# SUCCEEDS. With the old `|| echo 0` fallback a lock with no pid file read as permanently alive.
# Define this before the global preflight invokes build_is_running, not just before prune_lock.
owner_is_alive() {   # $1 = lock directory
  _pid=$(cat "$1/pid" 2>/dev/null || echo 0)
  _pgid=$(cat "$1/pgid" 2>/dev/null || echo 0)
  case "$_pid"  in ''|*[!0-9]*) _pid=0  ;; esac
  case "$_pgid" in ''|*[!0-9]*) _pgid=0 ;; esac
  [ "$_pid"  -gt 0 ] && kill -0 "$_pid" 2>/dev/null && return 0
  [ "$_pgid" -gt 0 ] && pgrep -g "$_pgid" >/dev/null 2>&1 && return 0
  return 1
}
# Split out because the two halves are answerable at DIFFERENT GRANULARITIES. A compiler runs in
# some checkout, so "which checkout" is a question with an answer (see `live_checkouts`). A held
# FFmpeg lock is about the SHARED cache, which belongs to no checkout at all — there is no finer
# answer to give, so this half stays all-or-nothing exactly as it was.
ffmpeg_lock_held() {
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
build_is_running() {
  for n in cargo rustc make cc1 arm-webos-linux-gnueabi-gcc; do
    if pgrep -x "$n" >/dev/null 2>&1; then echo "a running $n"; return 0; fi
  done
  ffmpeg_lock_held
}

# WHICH CHECKOUTS A BUILD IS LIVE IN — the per-checkout half of the liveness question
# `build_is_running` above answers globally.
#
# That global guard is right about one thing and wrong about another. It is right that a delete
# under a live `cargo` produces a CORRUPT tree rather than an absent one. It is wrong that the only
# safe response is to refuse EVERYTHING: on this machine several sessions run `make check` in
# different worktrees at once, so `pgrep -x make` essentially never comes back empty — and a
# reclaim tool that cannot run while anybody is building is one that cannot run on the day the
# volume fills, which is the only day it is reached for. Measured 2026-09-17: 5.0 GiB free, 34.8
# GiB of derived trees in IDLE lanes, and this script refusing to touch a byte of it because two
# unrelated lanes happened to be compiling.
#
# A builder's WORKING DIRECTORY names the checkout it is building in, so the question is
# answerable per checkout rather than per machine: collect every builder's cwd, map each back to
# the worktree CONTAINING it (a `cargo` run from `<lane>/rust-modules` must protect `<lane>`), and
# skip only those. Everything else is collected as before.
#
# Deliberately NOT an mtime test, which is the obvious wrong answer and was the first thing tried:
# a target directory's own mtime does not move while a compiler writes into `debug/deps` beneath
# it, so the tree being written to RIGHT NOW is precisely the one that reads as untouched for half
# an hour.
#
# It FAILS CLOSED ON IGNORANCE, and only on ignorance. If `lsof` is missing, or a builder is
# running and its cwd cannot be READ, this returns non-zero and the global refusal below stands
# unchanged — the same answer this script has always given, now reached only when the cheaper and
# more precise one is unavailable.
#
# A cwd that is read successfully and belongs to no checkout of this repository is a different
# thing, and is deliberately NOT a refusal: it is somebody's unrelated `cargo` in an unrelated
# project, and treating that as a reason to delete nothing here is precisely the all-or-nothing
# behaviour this function exists to replace — one Rust project open elsewhere on the machine would
# veto the whole reclaim. The case that looks like it and is not — a builder in a lane git has
# stopped listing, whose EXTERNAL target dir under $FLEET_DIR would otherwise be collected as an
# orphan — is answered by the second rule in `in_live_checkout`, not by refusing.
#
# Written with temp files rather than `$(…)` around the `case`, and that is not style: **bash 3.2
# — what macOS ships, and what `#!/bin/sh` resolves to here — cannot parse a `case` inside a
# command substitution at all.** It scans for the closing paren without parsing, so the `)` ending
# a case pattern terminates the substitution and the `;;` after it is a syntax error.
WT_LIST=
LIVE_LIST=
LIVE_CWDS=
# THE MOST SPECIFIC CHECKOUT CONTAINING A PATH. Not any containing checkout — this repository's
# lanes live INSIDE the main checkout (`<main>/.claude/worktrees/<lane>`), so a plain prefix test
# says every lane on the machine is part of main and one build in main protects all of them. The
# first run of this filter skipped five trees on the strength of two live builds for exactly that
# reason. The longest match is the owner.
#
# Second rule for the same question: a lane may point `CARGO_TARGET_DIR` at `$FLEET_DIR/<lane>`
# (that is what `fleet-plan` tells workers to do), which is under no checkout at all. Those are
# matched back the same way the orphan test matches them — by the lane directory's basename.
owning_checkout() {
  [ -n "$WT_LIST" ] && [ -s "$WT_LIST" ] || return 1
  _best=""
  while IFS= read -r w; do
    [ -n "$w" ] || continue
    case "$1" in
      "$w"|"$w"/*)
        if [ ${#w} -gt ${#_best} ]; then _best=$w; fi
        ;;
    esac
  done < "$WT_LIST"
  if [ -z "$_best" ] && [ -n "$FLEET_DIR" ]; then
    case "$1" in
      "$FLEET_DIR"/*)
        _lane=${1#"$FLEET_DIR"/}
        _lane=${_lane%%/*}
        while IFS= read -r w; do
          [ -n "$w" ] || continue
          if [ "${w##*/}" = "$_lane" ]; then _best=$w; fi
        done < "$WT_LIST"
        ;;
    esac
  fi
  [ -n "$_best" ] || return 1
  printf '%s\n' "$_best"
}
live_checkouts() {
  command -v lsof >/dev/null 2>&1 || return 1
  # OUR OWN ANCESTORS ARE NOT BUILDERS. The documented way to run this is `make disk`, which means
  # a `make` whose working directory IS the checkout being cleaned — so a naive cwd sweep protects
  # that checkout from the command the user typed to clean it, and the tree in front of them is the
  # one tree never collected. That `make` is not compiling anything; it is blocked waiting on this
  # script. Walk the parent chain and exclude it. A sibling `cargo` in the same checkout is still
  # found by its own cwd, so nothing real stops being protected.
  _anc=" "
  _p=$$
  _hops=0
  while [ "$_p" -gt 1 ] && [ "$_hops" -lt 32 ]; do
    _anc="$_anc$_p "
    _p=$(ps -o ppid= -p "$_p" 2>/dev/null | tr -d ' ') || _p=0
    case "$_p" in ''|*[!0-9]*) _p=0 ;; esac
    _hops=$((_hops + 1))
  done
  _pids=""
  for n in cargo rustc make cc1 arm-webos-linux-gnueabi-gcc; do
    _found=$(pgrep -x "$n" 2>/dev/null) || true
    for q in $_found; do
      case "$_anc" in *" $q "*) continue ;; esac
      _pids="$_pids $q"
    done
  done
  LIVE_LIST="${TMPDIR:-/tmp}/plx-build-gc-live.$$"
  : > "$LIVE_LIST"
  [ -n "$_pids" ] || return 0          # nothing building: an empty protected set is the truth
  LIVE_CWDS="${TMPDIR:-/tmp}/plx-build-gc-cwd.$$"
  _cwds=$LIVE_CWDS
  : > "$_cwds"
  # Per pid, and the distinction matters: a builder that EXITED between the `pgrep` above and the
  # `lsof` here is not a builder this run has to respect, while one that is still alive but whose
  # working directory cannot be read is exactly the ignorance this function fails closed on. An
  # empty answer alone cannot tell those apart — asking `kill -0` can.
  for p in $_pids; do
    _c=$(lsof -a -p "$p" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p') || true
    if [ -n "$_c" ]; then
      printf '%s\n' "$_c" >> "$_cwds"
    elif kill -0 "$p" 2>/dev/null; then
      rm -f "$_cwds"; LIVE_CWDS=; LIVE_LIST=; return 1
    fi
  done
  WT_LIST="${TMPDIR:-/tmp}/plx-build-gc-wt.$$"
  worktrees > "$WT_LIST"
  while IFS= read -r c; do
    [ -n "$c" ] || continue
    if _own=$(owning_checkout "$c"); then printf '%s\n' "$_own" >> "$LIVE_LIST"; fi
  done < "$_cwds"
  sort -u "$LIVE_LIST" -o "$LIVE_LIST"
  return 0
}
# Does this derived tree sit inside a checkout something is building in?
#
# Two rules, because a lane's build output does not have to be inside the lane. The second one is
# the fleet-teardown race: `fleet-plan` points a worker's `CARGO_TARGET_DIR` at
# `$FLEET_DIR/<lane>`, the lane's worktree is removed while its last build is still running, and
# `--orphans` — the mode this script tells you to reach for first after tearing a fleet down — then
# sees a target dir whose worktree is gone and deletes it out from under a live `cargo`. Rule 1
# cannot see that: the builder's cwd maps to the MAIN checkout (the removed lane directory sits
# under it), so main is protected and the external tree is not. Rule 2 asks the question that
# actually identifies it — is the lane's own name a path component of some live builder's working
# directory? — which holds whether or not git still lists the worktree.
#
# Rule 2 over-protects in principle: a lane named after a common directory component would spare an
# external tree that nobody is writing. That is the safe direction, and the tree is collected on the
# next run.
in_live_checkout() {
  if [ -n "$LIVE_LIST" ] && [ -s "$LIVE_LIST" ] && _own=$(owning_checkout "$1"); then
    grep -qxF "$_own" "$LIVE_LIST" && return 0
  fi
  [ -n "$LIVE_CWDS" ] && [ -s "$LIVE_CWDS" ] || return 1
  case "$1" in
    "$FLEET_DIR"/*) ;;
    *) return 1 ;;
  esac
  _lane=${1#"$FLEET_DIR"/}
  _lane=${_lane%%/*}
  [ -n "$_lane" ] || return 1
  while IFS= read -r c; do
    case "$c" in
      *"/$_lane"|*"/$_lane"/*) return 0 ;;
    esac
  done < "$LIVE_CWDS"
  return 1
}
# The filter every destructive mode pipes through. A skipped gigabyte is never silent.
cleanup_live() {
  [ -n "$LIVE_LIST" ] && rm -f "$LIVE_LIST"
  [ -n "$WT_LIST" ] && rm -f "$WT_LIST"
  [ -n "$LIVE_CWDS" ] && rm -f "$LIVE_CWDS"
  return 0
}
skip_live() {
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    if in_live_checkout "$d"; then
      printf '  in use, skipped  %s\n' "$d" >&2
    else
      printf '%s\n' "$d"
    fi
  done
}

# `--worktrees` — REMOVE A LANE OUTRIGHT, not just its build tree. Everything above this line
# reclaims what a worktree LEFT BEHIND; this is the mode for a worktree nobody needs anymore, and
# it exists because `git worktree list` at 128 entries (measured 2026-09-18) is not a report
# anyone reads by hand, and `git branch --merged` cannot see the answer at all — every lane here
# lands on `main` as a SQUASH (`AGENTS.md`, Working rules), so a lane branch's commits are never
# reachable from `main` by ancestry even when every byte they added is sitting on trunk. Instead,
# merge the lane tip into `main` with `git merge-tree --write-tree`; if the result equals
# `main^{tree}`, the lane adds nothing. The known limit: a lane whose lines `main` has since
# changed again conflicts and reads `unmerged` — the check errs toward keeping. (Measured
# 2026-09-18: 5 of 128 removable, 74 unmerged.)
#
# Two fields come off `git worktree list --porcelain`, and both need the multi-line record parsed
# rather than grepped line-by-line, because `locked` and `branch` are only PRESENT when true —
# their absence is exactly the fact being recorded, and a flat grep across the whole listing
# cannot attribute a `locked` line back to the worktree block it belongs to. Emits
# `path<TAB>sha<TAB>branch<TAB>locked` per worktree; `branch` is `(detached)` when there is none.
worktree_records() {
  printf '%s\n' "$WT_RAW" | awk '
    BEGIN { w="" }
    /^worktree / { if (w != "") print w "\t" sha "\t" branch "\t" locked
                   w=$0; sub(/^worktree /,"",w); sha=""; branch="(detached)"; locked="0"; next }
    /^HEAD /      { sha=$2; next }
    /^branch refs\/heads\// { b=$0; sub(/^branch refs\/heads\//,"",b); branch=b; next }
    /^locked/     { locked="1"; next }
    END { if (w != "") print w "\t" sha "\t" branch "\t" locked }
  '
}
# One word, or empty for "remove it". Order matters: cheapest and least surprising checks first,
# so a locked worktree reads as `locked` even if it also happens to be dirty. `building` protects
# a checkout a live compiler is writing into, the same guard `--lanes`/`--orphans` use via
# `in_live_checkout`.
worktree_reason() {
  _w=$1 _sha=$2 _locked=$3
  [ "$_w" = "$MAIN" ] && { echo main; return; }
  [ "$_w" = "$ROOT" ] && { echo current; return; }
  [ "$_locked" = "1" ] && { echo locked; return; }
  if in_live_checkout "$_w" 2>/dev/null; then echo building; return; fi
  if [ -n "$(git -C "$_w" status --porcelain 2>/dev/null)" ]; then echo dirty; return; fi
  if git -C "$_w" merge-base --is-ancestor "$_sha" main 2>/dev/null; then echo ""; return; fi
  # A detached HEAD that is an ancestor of main is caught above. Everything else — a branch tip,
  # detached or not — goes through the squash check: three-way merge `main` with `$_sha` and
  # compare the resulting tree to `main`'s own. No conflicts and no difference means every byte
  # the lane ever added is already on trunk, whether that arrived by squash-merge or by hand.
  _mt=$(git -C "$_w" merge-tree --write-tree main "$_sha" 2>/dev/null) || { echo unmerged; return; }
  _mt=$(printf '%s\n' "$_mt" | head -1)
  _maintree=$(git -C "$MAIN" rev-parse 'main^{tree}' 2>/dev/null)
  if [ -n "$_mt" ] && [ "$_mt" = "$_maintree" ]; then echo ""; else echo unmerged; fi
}

install_worktree_cargo_policy

if [ "$MODE" != report ]; then
  if [ -z "$DRY" ] && [ -z "$(worktrees)" ]; then
    echo "build-gc: cannot enumerate this repository's worktrees — refusing to delete anything." >&2
    echo "          Every reclaim mode decides what is dead from that list, so an empty one is a" >&2
    echo "          reason to stop, not a licence. Check that git works here and retry." >&2
    exit 1
  fi
  # The shared FFmpeg cache first, and unconditionally: a live `ci/build-ffmpeg.sh` is writing into
  # a tree that sits outside every checkout, so no per-checkout reasoning can exempt anything from
  # it. This is the refusal this script has always given for a held lock, unchanged.
  if [ -z "$DRY" ] && busy=$(ffmpeg_lock_held); then
    echo "build-gc: $busy — refusing to delete a build tree underneath it." >&2
    echo "          Wait for it, or re-run when the fleet is idle. (-n previews regardless.)" >&2
    exit 1
  fi
  # Ask the precise question first. `live_checkouts` succeeds when it could determine, for every
  # running builder, which checkout it is building in — in which case those checkouts are skipped
  # by name below and every idle one is collected. Only when it CANNOT tell does the old
  # all-or-nothing refusal apply, which is the honest reading of what this script can see then.
  if live_checkouts; then
    if [ -s "$LIVE_LIST" ]; then
      echo "build-gc: a build is live in these checkouts; their derived trees are left alone:"
      sed 's|^|  |' "$LIVE_LIST"
    fi
  elif [ -z "$DRY" ] && busy=$(build_is_running); then
    echo "build-gc: $busy, and this host cannot say which checkout it is building in" >&2
    echo "          (no lsof, or its working directory could not be read) — refusing to delete a" >&2
    echo "          build tree underneath it. Wait for it, or re-run when the fleet is idle." >&2
    echo "          (-n previews regardless.)" >&2
    exit 1
  fi
  trap cleanup_live EXIT
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
  echo "reclaim with: tools/build-gc.sh --orphans | --incremental | --cache | --lanes | --worktrees | --all   (add -n to preview)"
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
    external_incremental_trees; } | sort -u | skip_live | drop
  ;;
esac

CACHE_MAX_DAYS=${PLX_CACHE_MAX_DAYS-30}
# Take a cache entry's lock the same way `ci/build-ffmpeg.sh` does, INCLUDING its reclaim rule —
# otherwise the one thing guaranteed to leave a dead lock behind (a build that was killed) is also
# the thing that makes its tree permanently unprunable, so the entries most worth collecting are
# exactly the ones that never are. `mv` is the atomic claim; a lock under a minute old is left
# alone, because that is the window in which a live owner has not yet written its pid.
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
worktrees)
  echo "== finished linked worktrees (clean, unlocked, already on main; main and the running checkout are never touched) =="
  worktree_records | while IFS="$(printf '\t')" read -r w sha branch locked; do
    [ -n "$w" ] || continue
    reason=$(worktree_reason "$w" "$sha" "$locked")
    if [ -n "$reason" ]; then
      printf '  %-9s %s  (%s)\n' "$reason" "$w" "$branch"
      continue
    fi
    if [ -n "$DRY" ]; then
      printf '  would remove  %s  (%s)\n' "$w" "$branch"
      continue
    fi
    # No --force: a worktree this reached is already known clean, so a plain `remove` succeeding
    # is a second, independent confirmation of that — and if it somehow fails (a lock file, a
    # race with something else touching it this instant), refusing is the right answer, not
    # reaching for the flag that also eats uncommitted tracked changes (`fleet-plan`, §"Collecting
    # the work" measured this 2026-08-23: `--force` took a tree with modified tracked files and
    # all).
    if git worktree remove "$w" 2>&1; then
      printf '  removed       %s  (%s)\n' "$w" "$branch"
      ext="$FLEET_DIR/$(basename "$w")"
      if [ -d "$ext" ]; then
        if in_live_checkout "$ext" 2>/dev/null; then
          printf '    in use, skipped  %s\n' "$ext"
        else
          printf '%s\n' "$ext" | drop | sed 's/^/  /'
        fi
      fi
      case "$branch" in
        '(detached)') ;;
        *) printf '    branch left: %s\n' "$branch" ;;
      esac
    else
      echo "  FAILED to remove $w — see git's message above; left in place" >&2
    fi
  done
  git worktree prune >/dev/null 2>&1 || true
  ;;
esac

case "$MODE" in
orphans)
  echo "== external lane target dirs whose worktree is gone =="
  external_trees | sort -u | while IFS= read -r d; do
    if external_is_orphan "$d"; then echo "$d"; fi
  done | skip_live | drop
  ;;
esac

case "$MODE" in
all)
  echo "== external lane target dirs whose worktree is gone =="
  external_trees | sort -u | while IFS= read -r d; do
    if external_is_orphan "$d"; then echo "$d"; fi
  done | skip_live | drop
  ;;
esac

case "$MODE" in
lanes|all)
  echo "== derived trees in linked worktrees (the main checkout is left alone) =="
  worktrees | while IFS= read -r w; do
    [ "$w" = "$MAIN" ] && continue
    lane_trees "$w"
  done | skip_live | drop
  echo "== external lane build trees =="
  external_trees | sort -u | skip_live | drop
  ;;
esac

case "$MODE" in
all)
  echo "== the main checkout's vendor build trees (its target dirs are kept) =="
  vendor_trees "$MAIN" | skip_live | drop
  ;;
esac

if [ "$MODE" != report ]; then
  echo
  df -h "$ROOT" | tail -1 | awk '{print "free now: " $4 " of " $2}'
fi
