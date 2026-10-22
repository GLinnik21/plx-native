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
# iterates in; a lane's is cut for one task). The one exception that asks nothing is `--evict`, which
# may take the main checkout's `incremental` dirs, and only as the planner's last rung (a refusal
# otherwise); it never takes the main checkout's `target` itself. The other exception is `--worktrees`:
# it removes a whole finished checkout, and `git worktree remove` takes the checkout's IGNORED files with
# it, which can be private and hand-made (`src/config.local.h`, `pkg/auth.json`, a lane's notes). By hand
# it lists them; unattended (`--auto`) it removes only a checkout whose ignored files are all build output.
set -eu

# NO GIT CALL HERE MAY WRITE A LANE'S INDEX OR THE REPOSITORY'S OBJECT STORE. `git status` refreshes
# (rewrites) a worktree's index when a tracked file's stat data went stale, unless it is told the lock
# is optional: this script runs `status` in every lane it considers (`--worktrees`, and the inventory
# `--evict`'s plans come from), and a "report" that rewrites a lane's index races that lane's own
# `git add`. Exported so every git child, including the ones `tools/disk-budget.py` starts, inherits
# it. (`merge-tree --write-tree` writes objects, not the index: `worktree_reason` points it at a
# throwaway object directory.)
GIT_OPTIONAL_LOCKS=0
export GIT_OPTIONAL_LOCKS

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
# every agent (and, until `make check-nodef` replaced it, `AGENTS.md`'s own `--no-default-features`
# gate) invokes cargo directly, and a direct invocation never reads the Makefile. Measured 2026-09-17
# across this machine's 113 worktrees: **12.9 GB of `target*/debug/incremental` inside LINKED worktrees**, on a volume with
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
# formerly `[unstable] build-std`) mentions `[profile.dev]` debug. Cargo resolves a manifest `[profile]`
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
PLAN_FILE=
WANT_PLAN=
for a in "$@"; do
  if [ -n "$WANT_PLAN" ]; then PLAN_FILE=$a; WANT_PLAN=; continue; fi
  case "$a" in
    --report)      MODE=report ;;
    --evict)       MODE=evict; WANT_PLAN=1 ;;
    --evict=*)     MODE=evict; PLAN_FILE=${a#--evict=} ;;
    --incremental) MODE=incremental ;;
    --lanes)       MODE=lanes ;;
    --stale)       MODE=stale ;;
    --orphans)     MODE=orphans ;;
    --worktrees)   MODE=worktrees ;;
    --cache)       MODE=cache ;;
    --seed)        MODE=seed ;;
    --all)         MODE=all ;;
    --auto)        MODE=auto ;;
    -n|--dry-run)  DRY=1 ;;
    -h|--help)
      sed -n '2,/^set -eu/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'
      cat <<'USAGE'

usage: tools/build-gc.sh [MODE] [-n]

  (no mode)       report every checkout's derived trees, the shared cache and free space, then
                  the disk budget (tools/disk-budget.py report): each tree's PRIVATE bytes, the
                  PlxNative cargo trees under /tmp, and what a reclaim WOULD take, in order.
                  Read-only: it deletes nothing. `--evict` is the mode that acts on a plan
  --evict PLAN    delete what a PLAN names, and nothing else: PLAN is the file
                  `tools/disk-budget.py reclaim --to GIB --emit` prints (what to evict, in what
                  order, down to what free space). The one narrow deleting mode, and the one
                  `--auto` drives. Every line of the plan is
                  UNTRUSTED: the path must still BE a derived PlxNative build tree (a checkout's
                  `rust-modules/target*` or an `incremental` dir in one, a `target*` under
                  $PLX_FLEET_DIR, an interrupted delete's leftover under the build cache, or a
                  PlxNative cargo tree under /tmp or $TMPDIR, found by the planner's own
                  discovery, never a registered worktree's); a tracked file, `pkg/`, `vendor/`,
                  another project's tree, a symlink, the cargo seed, a live FFmpeg cache entry,
                  the main checkout's whole `target` and any rung the ladder does not make from
                  that kind of tree are REFUSED (exit 1). A tree that has turned live since the
                  plan was written is SKIPPED, not refused: it is re-probed immediately before
                  each rename (a builder in its checkout, a held cargo or `make check` lock, an
                  open file, the lane touched within the plan's idle window, a /tmp tree changed
                  within it by the later of mtime and ctime), and an `lsof` that cannot answer
                  counts as live. A removal is a rename to `<tree>.tombstone.<pid>.<n>` then
                  `rm -rf`, logged with what was freed and from whom. Unlike every other mode it
                  does NOT refuse for a held FFmpeg cache lock (it never writes there, and a full
                  disk is when an FFmpeg build is likeliest). It stops as soon as free space is
                  back over the plan's goal, runs the plan's `--stale` step at its own slot, and
                  ends with a `settle:` line: whether the free space it re-read covers the next
                  build's estimate plus the reserve (exit 3 when it does not). Needs python3 and
                  tools/disk-budget.py. `-n` prints what it would remove and logs nothing.
  --incremental   delete `target*/debug/incremental` everywhere. Always safe: it is a compile
                  cache. A linked worktree is not supposed to write one (the sanctioned
                  exceptions are the opt-in `make test-fast` and `make test-hostsim`, whose own
                  `target-fast` / `target-hostsim` this reclaims like any other; their next run
                  is a cold build) — but the Makefile's
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
                  is already on `origin/main` (the local `main` only where there is no origin;
                  `git merge-base --is-ancestor`, or a squash-merge check
                  via `git merge-tree --write-tree` against `main`'s tree — the check
                  `git branch --merged` cannot make, and the shape every lane here actually
                  lands as, per AGENTS.md's squash-only trunk rule). Removal is a plain
                  `git worktree remove` (no --force, so it refuses rather than eating anything
                  the clean-tree check missed) followed by that lane's external target dir under
                  $PLX_FLEET_DIR if one exists. Branches are never deleted; a removed worktree
                  whose branch still exists is reported as "branch left" so the owner decides.
                  Every worktree NOT removed is reported with one reason: dirty | locked |
                  unmerged | building | current | main | private.
                  `git worktree remove` takes the checkout's IGNORED files with it, and "clean"
                  cannot see them: a lane can hold `src/config.local.h`, `pkg/auth.json`,
                  `.tv-host`, its own notes or captures. By hand (a human typing `--worktrees`)
                  the removal stays, and the preview (`-n`) and the run list, under each lane,
                  every ignored path that is not build output `make` regenerates, so the human
                  sees what goes with it. Under `--auto` such a lane is KEPT instead (reason
                  `private`, the paths that kept it listed and written to the log), and so is one
                  whose ignored files git could not list; a lane whose ignored files are all
                  build output (cargo target dirs, the FFmpeg/Sentry/libass trees, what `make`
                  stages in `pkg/`, `__pycache__`, `.DS_Store`) is removed as before. The list is
                  `derived_output` in this script, an allow-list: an unclassified path keeps a lane.
  --stale         delete SUPERSEDED per-hash cargo artifacts from every `deps/` directory this
                  script can find — inside the lanes, inside the external $PLX_FLEET_DIR lane
                  target dirs, AND inside the MAIN checkout's own `rust-modules/target*` (the only mode
                  that reaches into that tree's `deps/`; `--incremental` and `--evict` take only
                  its `incremental` dirs). Cargo keys each build by a
                  metadata hash that folds in the exact feature/flag/profile combination (lib
                  test, `--no-default-features` check, devtools/hostsim, ARM); a source edit
                  reuses the SAME hash's files, but a hash nothing builds anymore — yesterday's
                  feature set, a reverted flag — keeps its binary, `.d`, `.rlib`/`.rmeta` and every
                  `*.rcgu.o` codegen-unit object forever, because cargo has no notion of "nobody
                  asks for this anymore" and never sweeps it. In a lane or an external fleet
                  target dir: keeps the newest hash per (crate name, artifact kind — test binary
                  vs rlib vs other) plus any hash whose newest file is younger than
                  $PLX_GC_STALE_HOURS hours (default 24); deletes every other hash's files
                  outright. In the MAIN checkout: a separate, more conservative rule — a hash is
                  only ever deleted for being OLDER than the threshold, and the newest hash per
                  crate (regardless of kind) is never touched, because that tree is the one a
                  human iterates in and a second live hash there is often deliberate. Either way,
                  once a test-binary hash survives, its own `*.rcgu.o` objects are still declined
                  individually once they age past the threshold — only a line-table backtrace
                  needs the binary; the objects exist solely for a debugger attached to it.
                  Respects the same in-use guard as every other mode: a `deps/` dir a live build
                  is writing into is skipped, never swept.
  --cache         delete shared FFmpeg build trees under $PLX_BUILD_CACHE untouched for
                  $PLX_CACHE_MAX_DAYS days (default 30). They are keyed by configure flags AND
                  toolchain, so a version bump or an NDK upgrade strands the old entry silently —
                  a cache nothing prunes is the same unbounded growth this script exists for.
                  The tarball is kept; it is 11 MB and every checkout copies from it. The same
                  threshold prunes the cargo seed (`tools/cargo-seed.py`) that fresh lanes are
                  cloned from, by the last time a lane restored from or refreshed it; a lane
                  built from it is unaffected (a clone owns its blocks), and the next lane to
                  finish a build makes a new one.
  --seed          delete the cargo seed (`tools/cargo-seed.py`) that fresh lanes are cloned from
                  when no lane has restored from or refreshed it for $PLX_SEED_MAX_DAYS days
                  (default 7). By hand only: neither `--auto` nor the planner ever evicts the
                  seed, because the lanes cloned from it share its blocks on APFS, so deleting
                  it frees almost nothing and makes every new lane's first build cold. The next
                  lane to finish a build makes a new one; lanes already seeded are unaffected.
  --all           --incremental everywhere plus --lanes, --stale, --orphans and --cache, and the
                  main checkout's vendor build trees. Leaves the main checkout's target dirs
                  themselves in place — `--stale` only removes the superseded hashes inside them.
  --auto          reclaim driven by free space on this volume, and the mode a hook or launchd job
                  runs unattended — see `tools/install-disk-watch.sh`. Always runs --orphans
                  first (cheap, safe, unconditional). Below $PLX_GC_MIN_FREE_GIB GiB free
                  (default 20): --worktrees restricted to lanes idle for at least
                  $PLX_GC_IDLE_MIN minutes (default 60, judged by the newest mtime anywhere under
                  the worktree and its fleet build tree, target dirs included — a lane an agent
                  might resume in the next few minutes is not the same as one nobody is touching,
                  and a needless rebuild costs real money) and holding no ignored file that is
                  not build output (a lane with `src/config.local.h`, `pkg/auth.json` or its own
                  notes is kept and the log says which paths kept it: nobody is there to read
                  what the removal takes); then, if still below, the PLANNER's
                  order: `tools/disk-budget.py reclaim --to $PLX_GC_MIN_FREE_GIB --emit` writes the
                  plan and `--evict` follows it (garbage, other lanes' incremental caches, younger
                  /tmp trees, whole idle lane trees, the `--stale` sweep, and the main checkout's
                  incremental dirs only as the last resort), stopping at the threshold plus 4 GiB
                  so the next tick does not reclaim again. A planner that fails or is missing
                  deletes nothing. What the five fixed stages it replaces did differently: the
                  main checkout's incremental cache and a lane's vendor build trees are no longer
                  cleared by default, the cargo seed is never evicted, and PlxNative trees left
                  under /tmp are now in scope. Never --cache, never a tracked file. Single-instance:
                  a mkdir lock (stale-safe, $PLX_GC_LOCK_DIR) means a second --auto exits 0 quietly
                  rather than racing the first. Logs one line per stage to $PLX_GC_LOG (default
                  ~/Library/Logs/plxnative-build-gc.log on macOS,
                  ${XDG_STATE_HOME:-~/.cache}/plxnative/build-gc.log elsewhere), truncated to its
                  last ~200 lines past ~1 MB. `--auto -n` previews every stage the current free
                  space would trigger and deletes nothing; it takes no lock.
  -n, --dry-run   print what would go, delete nothing.

Nothing here touches a tracked file or the television. The only thing it removes from the shared
cache under $PLX_BUILD_CACHE is what `--cache` finds unused for $PLX_CACHE_MAX_DAYS days (FFmpeg
trees and the cargo seed); `--auto` never runs `--cache` and never touches the seed.
Everything it removes is rebuilt by `make`.
USAGE
      exit 0 ;;
    *) echo "build-gc: unknown argument $a (try --help)" >&2; exit 2 ;;
  esac
done
if [ -n "$WANT_PLAN" ] || { [ "$MODE" = evict ] && [ -z "$PLAN_FILE" ]; }; then
  echo "build-gc: --evict needs a plan file (tools/disk-budget.py reclaim --to GIB --emit prints one)" >&2
  exit 2
fi

# --- `--auto` support ------------------------------------------------------------------------
# Everything below is DEFINITIONS ONLY (a shell function body is not evaluated until it is
# called), so it is safe to place these before `owner_is_alive`, `worktrees`, `fmt_kb` and the
# rest of the functions their bodies call — none of those run until `run_auto_mode` is invoked,
# far below, by which point every one of them has already been defined by the top-to-bottom pass
# through this script. What DOES have to happen this early is the single-instance lock check
# itself (see the `if [ "$MODE" = auto ]` gate just above the ordinary preflight, near the bottom
# of the argument handling): a second `--auto` must exit before it even enumerates worktrees or
# takes the FFmpeg lock, not after.
#
# `--auto` reclaims by SHELLING OUT to this same script, one mode at a time
# (`sh "$SELF" --orphans`, `sh "$SELF" --worktrees`, `sh "$SELF" --evict PLAN`), rather than
# reimplementing any mode's logic a second time. That is not laziness — `--worktrees`' squash-merge
# check, its live-build re-check immediately before `git worktree remove`, and `--evict`'s re-probe of
# every path before it renames one away are exactly the kind of logic that drifts the moment it is
# copied, and every one of those modes already carries its own preflight (the busy guard, the
# empty-enumeration refusal).
# Re-running that preflight once per stage costs a few `pgrep`/`lsof` calls, not a rebuild.
SELF="$ROOT/tools/build-gc.sh"

# Free space on the volume holding the repo, in KiB. `df -Pk` (POSIX output) rather than plain
# `df -k`: a long device name wraps GNU df's default format onto two lines, which would put the
# free-space column on a line `awk 'NR==2'` never sees. `PLX_GC_TEST_FREE_KIB` is a TEST-ONLY
# override — ci/test_build_gc.py stubs pressure without needing to fill a real disk.
free_kib() {
  if [ -n "${PLX_GC_TEST_FREE_KIB-}" ]; then printf '%s' "$PLX_GC_TEST_FREE_KIB"; return 0; fi
  df -Pk "$ROOT" 2>/dev/null | awk 'NR==2{print $4}'
}

# Where the auto log lives. macOS gets the platform's own log directory; anything else follows
# the XDG state dir, falling back to ~/.cache like every other dotfile-averse Linux tool here.
# `PLX_GC_LOG` overrides both, for a caller (or a test) that wants a known path.
gc_log_path() {
  if [ -n "${PLX_GC_LOG-}" ]; then printf '%s' "$PLX_GC_LOG"; return 0; fi
  case "$(uname -s 2>/dev/null)" in
    Darwin) printf '%s' "$HOME/Library/Logs/plxnative-build-gc.log" ;;
    *)
      if [ -n "${XDG_STATE_HOME-}" ]; then printf '%s' "$XDG_STATE_HOME/plxnative/build-gc.log"
      else printf '%s' "$HOME/.cache/plxnative/build-gc.log"; fi ;;
  esac
}
# One line per stage. Bounded rather than rotated: this is a diagnostic trail for a human to skim
# occasionally, not an audit log, so keeping the last ~200 lines once the file crosses ~1 MB is
# simpler than a rotation scheme and never leaves the disk problem this script exists for
# unbounded on the one file guaranteed to grow forever otherwise. Every step fails open — a log
# nobody can write to must not be why an unattended reclaim stops reclaiming.
gc_log() {
  lp=$(gc_log_path)
  [ -n "$lp" ] || return 0
  d=$(dirname "$lp")
  mkdir -p "$d" 2>/dev/null || return 0
  if [ -f "$lp" ]; then
    sz=$(wc -c <"$lp" 2>/dev/null | tr -d ' ')
    case "$sz" in ''|*[!0-9]*) sz=0 ;; esac
    if [ "$sz" -gt 1048576 ]; then
      tail -n 200 "$lp" >"$lp.tmp.$$" 2>/dev/null && mv "$lp.tmp.$$" "$lp" 2>/dev/null
    fi
  fi
  printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z' 2>/dev/null || date)" "$1" >>"$lp" 2>/dev/null || true
}

# Has anything under this lane's worktree — sources, staged files, the target dirs beneath it —
# been touched in the last $2 minutes? Used only to SPARE a lane: from `--worktrees` under `--auto`
# (PLX_GC_AUTO_IDLE_MIN), and from `--evict` for a lane other than the running one (`ev_lane_active`,
# the plan's own idle window) — and from `--lanes` when a human exports PLX_GC_AUTO_IDLE_MIN. Never
# to decide whether a build is running (that is `in_live_checkout`, which this
# does not replace). Deliberately a recursive `find -mmin`, not the directory's own mtime: a
# target dir's top-level entry does not move while a compiler writes beneath it — see
# `live_checkouts`' note on the identical trap — so the only honest answer is a walk. Piped into
# `head -1` so a single recent hit short-circuits the walk instead of statting a multi-gigabyte
# tree just to answer yes/no; `.git` is excluded because `git worktree remove` and routine git
# housekeeping touch it without the lane itself being active.
#
# `-mmin -0` is handled BEFORE ever calling `find`, not left for it to answer: "less than zero
# minutes old" ought to match nothing, and it did in isolated testing — but under real load
# (`ci/test_build_gc.py` measured this failing roughly every other run of its own idle-guard test,
# every failure showing `find` matching every file in the tree it should have matched none of) the
# exact zero boundary is not reliable across the (`stat`, `find`'s own clock read, filesystem mtime
# resolution) chain this walks. `$PLX_GC_IDLE_MIN=0` has an honest meaning anyway — "no idle guard,
# a lane is always eligible" — so answering it without `find` at all removes the flaky boundary
# case instead of trying to make it trustworthy. Non-numeric input is treated the same way: a
# caller-supplied guard this function cannot parse is not a reason to spare or to reclaim by
# accident, and "always eligible" is the same answer `run_auto_mode`'s own `case` gives a garbage
# `$PLX_GC_IDLE_MIN` before it ever reaches here.
idle_lane() {
  _w=$1 _mins=$2
  case "$_mins" in ''|*[!0-9]*) _mins=0 ;; esac
  [ "$_mins" -gt 0 ] || return 0
  if find "$_w" -mmin -"$_mins" -not -path '*/.git/*' -not -path '*/.git' 2>/dev/null \
       | head -1 | grep -q .; then
    return 1
  fi
  return 0
}

# The single-instance lock. mkdir is atomic even over NFS, which is the whole reason it is the
# mutex primitive everywhere else in this script (`prune_lock`, below). Stale-lock safe by the
# same protocol `prune_lock` uses for the FFmpeg cache: an unowned lock directory older than
# $PLX_GC_LOCK_STALE_MIN (default 120) is reclaimed rather than trusted forever, because the
# process most likely to leave a lock behind — one killed mid `--worktrees` `du` sweep, or a
# laptop that slept through it — is exactly the one that can never release it itself.
AUTO_LOCK_DIR=${PLX_GC_LOCK_DIR:-${TMPDIR:-/tmp}/plx-build-gc-auto.lock}
acquire_auto_lock() {
  if mkdir "$AUTO_LOCK_DIR" 2>/dev/null; then
    echo $$ >"$AUTO_LOCK_DIR/pid" 2>/dev/null || true
    ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' >"$AUTO_LOCK_DIR/pgid" || true
    return 0
  fi
  if ! owner_is_alive "$AUTO_LOCK_DIR" \
     && [ -n "$(find "$AUTO_LOCK_DIR" -maxdepth 0 -mmin +"${PLX_GC_LOCK_STALE_MIN:-120}" 2>/dev/null)" ]; then
    if mv "$AUTO_LOCK_DIR" "$AUTO_LOCK_DIR.stale.$$" 2>/dev/null; then rm -rf "$AUTO_LOCK_DIR.stale.$$"; fi
    if mkdir "$AUTO_LOCK_DIR" 2>/dev/null; then
      echo $$ >"$AUTO_LOCK_DIR/pid" 2>/dev/null || true
      ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' >"$AUTO_LOCK_DIR/pgid" || true
      return 0
    fi
  fi
  return 1
}
release_auto_lock() { rm -rf "$AUTO_LOCK_DIR" 2>/dev/null || true; }

# Run one stage as a fresh invocation of this script, log the free-space delta and reclaimed-line
# count, and echo its own report through — under `-n` a caller (human or the gate that runs
# `--auto -n`) is reading this output directly, and it must show every stage the current pressure
# would trigger, not just the log line.
run_auto_stage() {
  _name=$1; shift
  _before=$(free_kib); _before=${_before:-0}
  if [ -n "$DRY" ]; then _out=$(sh "$SELF" "$@" -n 2>&1) || true
  else _out=$(sh "$SELF" "$@" 2>&1) || true
  fi
  printf '%s\n' "$_out"
  _after=$(free_kib); _after=${_after:-0}
  # `|| true`: `grep -c` exits 1 on zero matches (the common case — nothing to reclaim), and under
  # `set -eu` an unguarded assignment from a failing command substitution kills the whole script.
  _n=$(printf '%s\n' "$_out" | grep -cE '  removed|would remove' || true)
  gc_log "$_name: reclaimed=$_n free $(fmt_kb "$_before")->$(fmt_kb "$_after")"
}

# The planner pass. `tools/disk-budget.py reclaim` decides WHAT to evict and in WHAT ORDER (the ladder
# in docs/disk-budget.md: garbage, then other lanes' incremental caches, then younger /tmp trees,
# then whole idle lane trees, then the `--stale` sweep, and the main checkout's incremental dirs
# only when nothing else is left); `--evict` is the only thing that deletes, and it re-checks every
# path against the live machine immediately before it renames it away. The plan is a temp file read
# once, so a planner that fails or is missing deletes NOTHING: a stage that cannot be planned is
# skipped and logged, never replaced by a guess.
#
# `--no-build`: nothing is about to build (this runs from a hook or a timer), so there is no current
# checkout to spare and no estimate to meet. `--to` is the threshold; the plan runs up to that plus
# 4 GiB of hysteresis, so the very next tick does not reclaim again, and `--evict` re-reads free
# space after every path and stops at that line.
run_auto_planner() {
  _planner="$ROOT/tools/disk-budget.py"
  if [ ! -f "$_planner" ] || ! command -v python3 >/dev/null 2>&1; then
    echo "auto: tools/disk-budget.py is not available here — the planner pass is skipped"
    gc_log "planner: unavailable, skipped"
    return 0
  fi
  _plan=$(mktemp "${TMPDIR:-/tmp}/plx-build-gc-plan.XXXXXX") || { gc_log "planner: no temp file, skipped"; return 0; }
  if python3 -B "$_planner" reclaim --to "$1" --no-build --idle-min "$2" --root "$ROOT" --emit \
       >"$_plan" 2>"$_plan.err" </dev/null; then
    run_auto_stage "evict (planner order)" --evict "$_plan"
  else
    echo "auto: the planner failed — nothing was evicted by it:"
    sed 's/^/  /' "$_plan.err"
    gc_log "planner failed: $(head -n 1 "$_plan.err" 2>/dev/null)"
  fi
  rm -f "$_plan" "$_plan.err"
}

# The orchestrator. `--orphans` runs first and unconditionally: it is the cheapest, safest mode this
# script has, reclaiming trees that literally nothing on the machine can refer to again, so there is
# no reason to gate it on pressure at all. Below the threshold: `--worktrees` (a finished lane
# outright, but only one idle for the guard window, as `--lanes` always demanded, and only one whose
# ignored files are all build output: nobody reads what the removal takes), then the
# planner's order. Under `-n` nothing is ever freed, so every stage the CURRENT pressure would
# trigger is shown, which is the honest reading of "preview the plan".
#
# What changed from the five fixed stages (--incremental, --worktrees, --lanes, --stale, --seed):
# the order is the planner's, so the main checkout's incremental cache and a lane's vendor build
# trees are no longer cleared by default (the former is the tree a human iterates in and waits for
# the last rung; the latter are not derived from cargo and are `--lanes`/`--all` only), the cargo seed
# is never evicted (its blocks are shared with the lanes cloned from it, so it frees almost nothing;
# `--seed` remains for hand use), PlxNative trees left under /tmp are now in scope, and the stop
# line is the threshold plus the hysteresis rather than the threshold itself.
run_auto_mode() {
  if [ -z "$DRY" ]; then
    if ! acquire_auto_lock; then
      gc_log "lock held ($AUTO_LOCK_DIR) — exiting quietly"
      return 0
    fi
    trap release_auto_lock EXIT
  fi

  _min_gib=${PLX_GC_MIN_FREE_GIB:-20}
  _idle_min=${PLX_GC_IDLE_MIN:-60}
  case "$_min_gib" in ''|*[!0-9]*) _min_gib=20 ;; esac
  case "$_idle_min" in ''|*[!0-9]*) _idle_min=60 ;; esac
  _min_kib=$((_min_gib * 1048576))

  _cur=$(free_kib); _cur=${_cur:-0}
  echo "== auto: planner-ordered reclaim (threshold ${_min_gib} GiB free, lane idle guard ${_idle_min}m) =="
  gc_log "start free=$(fmt_kb "$_cur") threshold=${_min_gib}GiB idle=${_idle_min}m dry=${DRY:-0}"

  run_auto_stage orphans --orphans
  _cur=$(free_kib); _cur=${_cur:-0}

  if [ "$_cur" -lt "$_min_kib" ] 2>/dev/null; then
    PLX_GC_AUTO_IDLE_MIN=$_idle_min
    export PLX_GC_AUTO_IDLE_MIN
    run_auto_stage "worktrees (idle >= ${_idle_min}m)" --worktrees
    unset PLX_GC_AUTO_IDLE_MIN
    [ -n "$DRY" ] || { _cur=$(free_kib); _cur=${_cur:-0}; }
  else
    echo "auto: free space already above threshold — worktrees and the planner pass skipped"
  fi
  if [ "$_cur" -lt "$_min_kib" ] 2>/dev/null; then
    run_auto_planner "$_min_gib" "$_idle_min"
    [ -n "$DRY" ] || { _cur=$(free_kib); _cur=${_cur:-0}; }
  fi

  gc_log "end free=$(fmt_kb "$_cur")"
  echo "auto: $(fmt_kb "$_cur") free now"
  return 0
}

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
# (target, -release, -sim, -lab, -sym, -macapp, -shots, -fast, -hostsim); the vendor entries are the FFmpeg and
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

# --- `--stale`: superseded per-hash cargo artifacts -------------------------------------------
#
# WHY THIS EXISTS. Every mode above deletes a whole CACHE (`--incremental`) or a whole TREE
# (`--lanes`, `--worktrees`); none of them look inside a live `target*/<profile>/deps` directory,
# because everything in there is nominally "still needed" — cargo put it there for a reason and
# will read some of it on the next build. What cargo does NOT do is ever forget a reason once it
# stops applying: each distinct feature/flag/profile combination (a plain `--lib` test build, the
# `--no-default-features` check gate, devtools/hostsim, the ARM cross build) gets its own metadata
# hash, a source edit only touches the files under the CURRENT hash, and a hash nothing builds
# anymore — yesterday's feature set, a reverted flag — keeps its binary and every `*.rcgu.o`
# codegen-unit object beside it forever. Measured 2026-09-28 in the main checkout:
# `rust-modules/target/debug/deps` held 6416 `plxnative_modules-*` files across 6 live-SIZED
# hashes (~100 MB binary + objects each), all last written 2026-09-17 — 5.5 GB nothing had built
# from in eleven days.
#
# THE UNIT OF DECISION IS THE METADATA HASH, NOT THE FILE. Every artifact cargo writes for one
# compilation shares the same `<crate>-<hash>` prefix: the test/check binary itself
# (`<crate>-<hash>`, executable, no extension), its dep-info (`<crate>-<hash>.d`), the rlib side
# (`lib<crate>-<hash>.rlib`/`.rmeta`/`.a`/`.so`/`.dylib`), and — with this host's default
# `split-debuginfo=unpacked` — one `<crate>-<hash>.<codegen-hash>-cgu.NN.rcgu.o` per codegen unit,
# which is where nearly all the bulk lives (250-2300 files per hash, measured). Keeping or
# deleting anything less than the whole hash-group at once produces a `.d` file with no binary or
# an `.rlib` with half its objects gone — not smaller, just broken.
PLX_GC_STALE_HOURS_DEFAULT=24
STALE_HOURS=${PLX_GC_STALE_HOURS:-$PLX_GC_STALE_HOURS_DEFAULT}
case "$STALE_HOURS" in ''|*[!0-9]*) STALE_HOURS=$PLX_GC_STALE_HOURS_DEFAULT ;; esac
STALE_SECS=$((STALE_HOURS * 3600))
# A 16-hex-digit run, spelled out one class per position rather than `{16}`: `/bin/sh` here
# resolves to whatever ships as `awk`, and this script already assumes the BSD/macOS toolchain
# elsewhere (see the `stat -f` fallbacks below) — an interval expression is not guaranteed
# portable across every awk this might run under, and a fixed hash width is guaranteed by rustc.
HASH16_RE='[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'
file_mtime() { stat -f%m "$1" 2>/dev/null || stat -c%Y "$1" 2>/dev/null; }
file_size()  { stat -f%z "$1" 2>/dev/null || stat -c%s "$1" 2>/dev/null; }

# Every `deps/` directory under one `target*` root — covers a plain `target/debug/deps` as well
# as a cross-build's `target/<triple>/debug/deps`, and every other feature-set root this script
# already globs for (`target-sim`, `target-lab`, ...).
stale_deps_dirs() {
  find "$1" -type d -name deps 2>/dev/null
}
# The cargo target* roots directly under a worktree's `rust-modules/`, or (for a lane whose
# CARGO_TARGET_DIR points at $FLEET_DIR/<lane>) directly under the lane directory itself.
target_roots() {
  for d in "$1"/rust-modules/target*; do
    case "$d" in *'*'*) continue ;; esac
    if [ -d "$d" ]; then echo "$d"; fi
  done
}
external_target_roots() {
  for d in "$1"/target*; do
    case "$d" in *'*'*) continue ;; esac
    if [ -d "$d" ]; then echo "$d"; fi
  done
}

# THE SWEEP ITSELF, one `deps/` directory at a time. `$2` is the grouping scope: `kind` (lanes and
# external fleet trees) keeps the newest hash per (crate name, artifact kind: test binary vs rlib
# vs other) — the aggressive rule, appropriate for a tree that is rebuilt on demand anyway. `crate`
# (the main checkout only) keeps the newest hash per crate name ALONE, never mind kind, which is
# the more conservative floor: a human iterates in this tree and a second live hash there — a
# `--no-default-features` check run beside the ordinary one — is often deliberate. Both scopes
# additionally keep any hash whose newest file is younger than $PLX_GC_STALE_HOURS hours, and both
# still decline a KEPT test-binary hash's individual `*.rcgu.o` objects once those age past the
# threshold: only a line-table backtrace needs the binary; the objects exist solely for a debugger
# attached to it, and nothing here attaches one to a superseded-in-spirit-but-still-newest binary.
#
# Two passes, both in one awk program fed `mtime\tsize\texec\tpath` lines (so nothing here builds
# an associative array by hand in `/bin/sh`, which cannot): accumulate every file under its
# `<crate>\t<hash>` key while scanning, then in `END` decide per hash and emit either `DEL\t<path>`
# lines (one per file to remove) or a `SUM\t<label>\t<count>\t<bytes>` line just before the DEL
# lines for that hash, so the shell loop below can print one report line per hash-group instead of
# one per file — the object-file count alone would otherwise flood the terminal.
run_stale_sweep_dir() {
  _dd=$1 _scope=$2
  find "$_dd" -maxdepth 1 -type f 2>/dev/null | while IFS= read -r f; do
    _mt=$(file_mtime "$f") || continue
    case "$_mt" in ''|*[!0-9]*) continue ;; esac
    _sz=$(file_size "$f"); case "$_sz" in ''|*[!0-9]*) _sz=0 ;; esac
    if [ -x "$f" ]; then _ex=1; else _ex=0; fi
    printf '%s\t%s\t%s\t%s\n' "$_mt" "$_sz" "$_ex" "$f"
  done | awk -v scope="$_scope" -v now="$(date +%s)" -v thresh="$STALE_SECS" -v hexre="^[A-Za-z0-9_]+-$HASH16_RE" '
    BEGIN { FS="\t" }
    {
      mtime = $1 + 0; size = $2 + 0; exe = $3 + 0; path = $4
      n = split(path, parts, "/"); base = parts[n]
      islib = (base ~ /^lib/)
      core = islib ? substr(base, 4) : base
      if (!match(core, hexre)) next
      key = substr(core, RSTART, RLENGTH)
      hlen = length(key)
      name = substr(key, 1, hlen - 17)
      hash = substr(key, hlen - 15)
      grp = name SUBSEP hash
      is_obj = (base ~ /\.rcgu\.o$/)
      is_bin = (!islib && core == key && exe == 1)
      is_libart = islib && (base ~ /\.rlib$/ || base ~ /\.rmeta$/ || base ~ /\.a$/ || \
                             base ~ /\.so$/ || base ~ /\.dylib$/)
      if (is_bin) hkind[grp] = "bin"
      else if (is_libart && hkind[grp] != "bin") hkind[grp] = "rlib"
      else if (!(grp in hkind)) hkind[grp] = "other"
      if (!(grp in hnewest) || mtime > hnewest[grp]) hnewest[grp] = mtime
      hname[grp] = name; hhash[grp] = hash
      c = ++fcount[grp]
      fpath[grp, c] = path; fsize[grp, c] = size; fmtime[grp, c] = mtime; fisobj[grp, c] = is_obj
      totalsize[grp] += size
    }
    END {
      for (g in hnewest) {
        gk = (scope == "kind") ? (hname[g] SUBSEP hkind[g]) : hname[g]
        if (!(gk in gbestnewest) || hnewest[g] > gbestnewest[gk]) { gbestnewest[gk] = hnewest[g]; gbest[gk] = g }
      }
      for (g in hnewest) {
        gk = (scope == "kind") ? (hname[g] SUBSEP hkind[g]) : hname[g]
        keepnewest = (gbest[gk] == g)
        young = ((now - hnewest[g]) < thresh)
        label = hname[g] "-" hhash[g]
        if (keepnewest || young) {
          if (hkind[g] == "bin") {
            declsize = 0; declcount = 0
            for (i = 1; i <= fcount[g]; i++) {
              if (fisobj[g, i] && (now - fmtime[g, i]) >= thresh) {
                declsize += fsize[g, i]; declcount++
                print "DEL\t" fpath[g, i]
              }
            }
            if (declcount > 0) print "SUM\t" label " rcgu.o objects\t" declcount "\t" declsize
          }
        } else {
          print "SUM\t" label "\t" fcount[g] "\t" totalsize[g]
          for (i = 1; i <= fcount[g]; i++) print "DEL\t" fpath[g, i]
        }
      }
    }
  ' | {
    _tab=$(printf '\t')
    while IFS="$_tab" read -r _tag _a _b _c; do
      case "$_tag" in
        SUM)
          _kb=$((${_c:-0} / 1024))
          if [ -n "$DRY" ]; then printf '  would remove %8s  %s (%s files) in %s\n' "$(fmt_kb "$_kb")" "$_a" "$_b" "$_dd"
          else printf '  removed %8s  %s (%s files) in %s\n' "$(fmt_kb "$_kb")" "$_a" "$_b" "$_dd"; fi
          ;;
        DEL)
          if [ -z "$DRY" ]; then rm -f "$_a"; fi
          ;;
      esac
    done
  }
}

# AN EXPORTED `CARGO_TARGET_DIR` IS LISTED AND NEVER TARGETED, and the asymmetry is the whole
# point. A path from the environment is arbitrary: it can be a directory shared by several
# checkouts, it can be somewhere entirely unrelated to this project, and its PARENT's basename —
# which is how an external tree is matched back to a lane — means nothing at all, so the orphan
# test would classify it from a coincidence. Deleting a tree BECAUSE the environment names it would
# be exactly the "preserve unrelated generated artifacts" rule this repository states outright. The
# destructive scope is therefore $FLEET_DIR, whose layout this script defines, plus (for `--evict`
# only) the planner's own /tmp discovery: a cargo tree under /tmp or $TMPDIR that carries a PlxNative
# marker and has sat idle past the plan's window. An exported tree that happens to be one of those is
# found by that rule, never by the export; anything else is shown so you can decide for yourself.
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
builders_running() {
  for n in cargo rustc make cc1 arm-webos-linux-gnueabi-gcc; do
    if pgrep -x "$n" >/dev/null 2>&1; then echo "a running $n"; return 0; fi
  done
  return 1
}
build_is_running() {
  builders_running && return 0
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
  # ONE read of the process table and one awk, however deep the chain: a `ps -p` per hop was ten-odd
  # process spawns per stage, in every stage of an `--auto`, and a spawn is the expensive thing on a
  # loaded machine. A `ps` that fails leaves just this process, the same answer the per-hop form gave.
  _anc=" $(ps -axo pid=,ppid= 2>/dev/null | awk -v me="$$" '
    { parent[$1] = $2 }
    END { p = me; for (h = 0; p > 1 && h < 32; h++) { printf "%s ", p; p = parent[p] + 0 } }') "
  case "$_anc" in *" $$ "*) ;; *) _anc=" $$ " ;; esac
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
#
# WHAT "ON MAIN" MEANS: `origin/main` when this repository has one, else the local `main`. The local
# branch is only as new as the last `git merge` someone ran, and on a machine where lanes land by
# squash on GitHub it is days behind: judged against it every lane that has landed reads `unmerged`
# and is kept forever (measured 2026-10-19: the report's "merged" lanes were all "unmerged" here).
# `tools/disk-budget.py` makes the same choice (`base_ref`), so the report and this mode agree. It
# is resolved ONCE, at top level, because this function runs inside `worktree_records | while`, a
# subshell whose assignments would not survive. Empty when there is neither: nothing reads merged.
resolve_base_ref() {
  for _r in refs/remotes/origin/main refs/heads/main; do
    if git -C "$MAIN" rev-parse --verify --quiet "$_r^{commit}" >/dev/null 2>&1; then
      printf '%s' "$_r"; return 0
    fi
  done
  return 0
}
BASE_REF=
worktree_reason() {
  _w=$1 _sha=$2 _locked=$3
  [ "$_w" = "$MAIN" ] && { echo main; return; }
  [ "$_w" = "$ROOT" ] && { echo current; return; }
  [ "$_locked" = "1" ] && { echo locked; return; }
  if in_live_checkout "$_w" 2>/dev/null; then echo building; return; fi
  if [ -n "$(git -C "$_w" status --porcelain 2>/dev/null)" ]; then echo dirty; return; fi
  [ -n "$BASE_REF" ] || { echo unmerged; return; }
  if git -C "$_w" merge-base --is-ancestor "$_sha" "$BASE_REF" 2>/dev/null; then echo ""; return; fi
  # A detached HEAD that is an ancestor of the base is caught above. Everything else — a branch tip,
  # detached or not — goes through the squash check: three-way merge the base with `$_sha` and
  # compare the resulting tree to the base's own. No conflicts and no difference means every byte
  # the lane ever added is already on trunk, whether that arrived by squash-merge or by hand.
  # `merge-tree --write-tree` WRITES the merged tree (and any merged blob) into the object store, and
  # this runs for every lane that is not an ancestor, on every `--worktrees` and so every hourly
  # `--auto`: against the real store that is a loose object per unmerged lane per change on either
  # side, kept until a `git gc`. So it runs against a throwaway object directory with the real one as
  # an alternate to read from (`tools/disk-budget.py`'s `is_merged` does the same). The tree id it
  # prints is all that is kept.
  _objs=$(git -C "$_w" rev-parse --path-format=absolute --git-path objects 2>/dev/null) || { echo unmerged; return; }
  _scratch=$(mktemp -d "${TMPDIR:-/tmp}/plx-build-gc-merge.XXXXXX" 2>/dev/null) || { echo unmerged; return; }
  _mt=$(GIT_OBJECT_DIRECTORY=$_scratch GIT_ALTERNATE_OBJECT_DIRECTORIES=$_objs \
        git -C "$_w" merge-tree --write-tree "$BASE_REF" "$_sha" 2>/dev/null) || { rm -rf "$_scratch"; echo unmerged; return; }
  rm -rf "$_scratch"
  _mt=$(printf '%s\n' "$_mt" | head -1)
  _maintree=$(git -C "$MAIN" rev-parse "$BASE_REF^{tree}" 2>/dev/null)
  if [ -n "$_mt" ] && [ "$_mt" = "$_maintree" ]; then echo ""; else echo unmerged; fi
}

# IGNORED FILES GO WITH A WORKTREE. `git worktree remove` deletes everything in the checkout that git does
# not track, ignored files included, and "working tree clean" (`worktree_reason`'s `dirty` test) does not see
# them: a lane can hold `src/config.local.h` (the real Plex token), `tests/manifest.local.json`, `pkg/auth.json`,
# `pkg/lab.json`, `.tv-host`, `.tv-mac` and the other gitignored private files `.claude/hooks/outbound-guard.py`
# lists, or its own notes and captures, none of which `make` can rebuild. Before the squash check read
# `origin/main` this never fired for a squash-merged lane; now `--auto` reaches them unattended, hourly.
#
# `derived_output` says whether one IGNORED path (relative to the worktree, no trailing slash) is output that
# `make` or a tool regenerates from tracked sources. It is an ALLOW-list on purpose: a path nobody classified
# is NOT build output, and the failure of a missing entry is a lane that stays (logged, with the path) until
# someone looks, where the failure of a wrong entry is a deleted token. The source is `.gitignore` itself, whose
# sections say which of their entries are derived; the first line is the `lane_trees` glob and the vendor line
# covers `vendor_trees` and the sibling trees that function does not list (`--lanes` reclaims a tree's CONTENTS,
# this asks whether the path is only ever output). `ci/test_build_gc.py` walks every `.gitignore` pattern and fails
# on one that is neither listed here nor named there as kept, so the two cannot drift. `pkg/*` is deliberately
# NOT a pattern: `pkg/auth.json`, `pkg/lab.json`, `pkg/telemetry.local.json` and `pkg/diagnostics/` live there.
# Kept on purpose, though cheap to refetch: what was PULLED FROM THE TV (`.abi-cache/`, `sysroot/`,
# `tests/fixtures/softfloat/arm.tbl`), which needs the one television to get back.
derived_output() {
  case "$1" in
    rust-modules/target*) ;;                                    # every feature set's cargo target dir
    vendor/ffmpeg-*|vendor/sentry-native-*|vendor/libass-*|vendor/downloads|vendor/nanosvg|vendor/vendor) ;;
    pkg/plxnative|pkg/plxnative-storage|pkg/plxnative-stackwalk|pkg/plxnative.debug|pkg/threadprobe) ;;
    pkg/mali-irq-sample|pkg/sockprobe|pkg/logmprobe) ;;
    pkg/*.ipk|pkg/ipk.sha256|pkg/lib*-plx.so.*|pkg/libass-plx-host.so.0*|pkg/libass-plx.0.dylib.*.new) ;;
    pkg/*.dylib|pkg/symbols|pkg/.ffabi-ok|pkg/.ffabi-host-ok|pkg/.build-config|pkg/.config-local-stamp) ;;
    pkg/.flavor|pkg/PlxNative.app|pkg/PlxNative-*|pkg/*.link.*|pkg/link-evidence) ;;
    ipkroot/*.tar.gz|ipkroot/debian-binary|ipkroot/data/usr|stub/*.so|*.o) ;;
    tools/node_modules|__pycache__|*/__pycache__|*.pyc|.DS_Store|*/.DS_Store) ;;
    .tv-*.pid|.tv-*.log) ;;                                     # a session's scratch, not its address or password
    tests/fixtures/out|tests/fixtures/*.mkv|tests/fixtures/*.mp4|tests/fixtures/*.sup|tests/fixtures/fixtures.json) ;;
    *) return 1 ;;
  esac
  return 0
}
# The ignored paths in worktree `$1` that are NOT build output, one per line (empty: removing it takes nothing
# `make` cannot rebuild). `--ignored=matching` names each ignored path at the granularity of the pattern that
# ignores it (`vendor/downloads/`, not a `vendor/` that merely contains it), and a directory is never walked, so
# a multi-gigabyte `target/` costs one line. Returns 1 when git cannot answer — the caller must not read that
# as "nothing". A path git quotes (spaces) never matches `derived_output`, so it is listed: kept, not guessed at.
private_ignored_paths() {
  _pst=$(git -C "$1" -c core.quotePath=false status --porcelain --ignored=matching 2>/dev/null) || return 1
  printf '%s\n' "$_pst" | sed -n 's/^!! //p' | while IFS= read -r _pp; do
    _pp=${_pp%/}
    if ! derived_output "$_pp"; then printf '%s\n' "$_pp"; fi
  done
}
# `$1` heading, `$2` newline-separated paths: at most fifty under the heading (a `*.log` pattern can ignore
# thousands of files), the rest as a count.
print_paths() {
  printf '%s\n' "$2" | awk -v h="$1" 'NR <= 50 { if (NR == 1) print h; print "      " $0 }
                                      END { if (NR > 50) printf "      ... and %d more\n", NR - 50 }'
}
# By hand: what removing the worktree takes that `make` cannot rebuild, or that git could not say. Reads the
# `--worktrees` loop's `_priv` / `_priv_ok`; `$1` is the heading.
print_private_note() {
  if [ -z "$_priv_ok" ]; then printf '      git could not list its ignored files\n'
  elif [ -n "$_priv" ]; then print_paths "$1" "$_priv"; fi
}
# The same for the log: one line, the first five.
join_paths() {
  printf '%s\n' "$1" | awk 'NR <= 5 { s = s (NR > 1 ? ", " : "") $0 }
                            END { if (NR > 5) s = s sprintf(", ... and %d more", NR - 5); print s }'
}

install_worktree_cargo_policy
BASE_REF=$(resolve_base_ref)

# --- `--evict PLAN`: the ONE narrow deleting mode the planner drives ---------------------------------
#
# `tools/disk-budget.py reclaim --to GIB --emit` writes a plan (what to evict, in what order, with
# what free-space goal; the format is `emit` in that file). THIS is the only code that deletes from
# it, and it treats the plan as UNTRUSTED INPUT: a stale plan, a hand-edited plan and a plan from a
# future planner must all fail safe. Every line is therefore re-derived from the machine, not read:
#
#   1. WHAT IT IS. The path is resolved physically and must BE one of: a registered checkout's
#      `rust-modules/target*` (or an `incremental` dir inside one), a `target*` under $PLX_FLEET_DIR,
#      an interrupted delete's leftover under the build cache, or a PlxNative cargo tree found by the
#      planner's own `/tmp` discovery (`disk-budget.py tmp-trees`: one scanner, not two). Anything
#      else (a tracked file, `pkg/`, `vendor/`, a non-PlxNative tree, a symlink, a path nowhere near
#      a checkout, a live FFmpeg cache entry, the cargo seed) is REFUSED, loudly, and the run exits 1.
#      `target_roots`, not `lane_trees`: the latter includes the vendor build trees.
#   2. WHAT RUNG IT MAY BE. (class, rung, scope) must be an eviction the ladder can make: the main
#      checkout only gives up an `incremental` dir at rung 6; the checkout this runs from only a
#      rung-6 `incremental` in mode `current` (a rung 0/2/4 line naming it is skipped as spared);
#      whole trees only from other lanes.
#   3. WHETHER IT IS LIVE, asked again IMMEDIATELY BEFORE the rename, per path: a builder running in
#      its checkout (`live_checkouts`, refreshed every time), and `disk-budget.py probe`: a held
#      cargo lock, a held per-checkout `make check` lock, an open file under the tree, a tracked file,
#      and for a /tmp tree its age by the LATER of mtime and ctime (a fixture backdated with `utime`
#      is not old). Lane-level idleness (`idle_lane`, the whole worktree) is judged for OTHER lanes
#      only, and not again for a lane this run has already taken something from, whose directory
#      mtimes this run itself just moved. A live tree is skipped, not refused: the plan was right
#      when it was written.
#   4. Rename to `<tree>.tombstone.<pid>.<n>` (a sibling of the tree root, so a later sweep finds an
#      interrupted delete), then `rm -rf`. A log line says what was freed and from whom.
#
# It does NOT take the machine-wide FFmpeg-lock refusal the other modes begin with (docs/
# disk-budget.md, "Concurrency"): it never touches the FFmpeg cache's live entries (bar a tombstone) —
# it does remove day-old cargo-seed leftovers — so a build in there is not a reason to leave a full disk
# full. After every path it re-reads free space and stops at the plan's goal; the
# plan's `stale` line runs `--stale` in its own rung-5 slot; a last `settle:` line says whether the
# free space it re-read covers the build's estimate plus the reserve.
EV_WT_FILE=
EV_MAIN=
EV_ROOT=
EV_CLASS=
EV_TREE=
EV_CHECKOUT=
EV_LANE=
EV_WHY=
EV_SPARE=
EV_TMP_TREES=
EV_TMP_LOADED=
EV_TOUCHED="|"
EV_N=0
EV_DONE=0
EV_SKIPPED=0
EV_REFUSED=0
EV_FREED_KIB=0
EV_EXIT=0

ev_py() { python3 -B "$ROOT/tools/disk-budget.py" "$@" </dev/null; }
ev_cleanup() { cleanup_live; if [ -n "$EV_WT_FILE" ]; then rm -f "$EV_WT_FILE"; fi; }
ev_refuse() {
  printf '  REFUSED          %s  (%s)\n' "$1" "$2" >&2
  EV_REFUSED=$((EV_REFUSED + 1))
}
ev_skip() {
  printf '  in use, skipped  %s  (%s)\n' "$1" "$2"
  EV_SKIPPED=$((EV_SKIPPED + 1))
}
# The physical path of an existing directory (symlinks resolved), or non-zero.
ev_phys() { [ -n "$1" ] || return 1; (cd -P "$1" 2>/dev/null && pwd -P); }

# Is $1 the tree root $2 itself, or an `incremental` dir one or two levels below it
# (`<tree>/<profile>/incremental`, `<tree>/<triple>/<profile>/incremental`)? Nothing deeper.
ev_within() {
  [ "$1" = "$2" ] && return 0
  case "$1" in "$2"/*) ;; *) return 1 ;; esac
  _rel=${1#"$2"/}
  case "$_rel" in */*/*/*) return 1 ;; esac
  case "$_rel" in */incremental) return 0 ;; esac
  return 1
}

# Sets EV_CLASS (lane|main|fleet|tmp|leftover), EV_TREE (the tree root the path belongs to),
# EV_CHECKOUT (the owning checkout, physical) and EV_LANE (a fleet tree's lane name); returns
# non-zero when the path is none of them.
ev_classify() {
  _real=$1
  EV_CLASS= EV_TREE= EV_CHECKOUT= EV_LANE=
  while IFS= read -r _w; do
    [ -n "$_w" ] || continue
    _wp=$(ev_phys "$_w") || continue
    for _t in "$_wp"/rust-modules/target*; do
      case "$_t" in *'*'*) continue ;; esac
      if [ -d "$_t" ] && ev_within "$_real" "$_t"; then
        EV_TREE=$_t
        EV_CHECKOUT=$_wp
        if [ "$_wp" = "$EV_MAIN" ]; then EV_CLASS=main; else EV_CLASS=lane; fi
        return 0
      fi
    done
  done < "$EV_WT_FILE"
  if [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ]; then
    _fp=$(ev_phys "$FLEET_DIR") || _fp=
    if [ -n "$_fp" ]; then
      for _t in "$_fp"/*/target*; do
        case "$_t" in *'*'*) continue ;; esac
        if [ -d "$_t" ] && ev_within "$_real" "$_t"; then
          EV_TREE=$_t
          EV_CLASS=fleet
          EV_LANE=${_t#"$_fp"/}
          EV_LANE=${EV_LANE%%/*}
          while IFS= read -r _w; do
            [ -n "$_w" ] || continue
            if [ "${_w##*/}" = "$EV_LANE" ]; then EV_CHECKOUT=$(ev_phys "$_w") || EV_CHECKOUT=; fi
          done < "$EV_WT_FILE"
          return 0
        fi
      done
    fi
  fi
  _cache=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
  if [ -n "$_cache" ] && [ -d "$_cache" ]; then
    _cp=$(ev_phys "$_cache") || _cp=
    case "$_real" in
      "$_cp"/cargo-seed/*.tmp.*|"$_cp"/cargo-seed/*.restore.*|"$_cp"/cargo-seed/*.old.*|"$_cp"/cargo-seed/*.tombstone.*|"$_cp"/ffmpeg/*.tombstone.*)
        _rest=${_real#"$_cp"/}
        case "$_rest" in
          */*/*) ;;
          *) EV_CLASS=leftover; EV_TREE=$_real; return 0 ;;
        esac ;;
    esac
  fi
  if [ -z "$EV_TMP_LOADED" ]; then
    EV_TMP_TREES=$(ev_py tmp-trees --root "$ROOT" 2>/dev/null) || EV_TMP_TREES=
    EV_TMP_LOADED=1
  fi
  if [ -n "$EV_TMP_TREES" ] && printf '%s\n' "$EV_TMP_TREES" | grep -qxF "$_real"; then
    EV_CLASS=tmp
    EV_TREE=$_real
    return 0
  fi
  return 1
}

# May a tree of class $1 give up `$3` at rung `$2`, in mode `$4`? Sets EV_WHY when not, and EV_SPARE when the
# line is a fair eviction in itself but not from the checkout this runs from (skipped, not refused).
ev_allowed() {
  _c=$1 _r=$2 _s=$3 _m=$4 _p=$5
  EV_SPARE=
  # An interrupted delete is a tree ROOT named `<tree>.tombstone.<pid>.<n>`; the mark is read from the last
  # path component only, so a checkout that happens to have it further up its path gets no such licence.
  case "${_p##*/}" in
    *.tombstone.*)
      if [ "$_r:$_s" = "0:tree" ]; then return 0; fi
      EV_WHY="an interrupted delete is only ever a rung-0 tree"; return 1 ;;
  esac
  # The checkout this runs FROM is the one about to build (a build-entry preflight runs here), so it
  # gives up exactly one thing: a rung-6 `incremental` dir, in mode `current`. Its own tree at rungs 0,
  # 2 or 4 is what the planner never writes for a build (the current checkout is excluded from them) and
  # would be pure waste to take; `reclaim --no-build` run by hand from a lane can name it, though, so
  # that line is SPARED (skipped, EV_SPARE) rather than refused. Lane and fleet trees both belong to a
  # checkout (a fleet tree is its lane's CARGO_TARGET_DIR kept outside the worktree).
  case "$_c:$_r:$_s" in
    lane:0:tree|lane:2:incremental|lane:4:tree|fleet:0:tree|fleet:2:incremental|fleet:4:tree)
      if [ -n "$EV_CHECKOUT" ] && [ "$EV_CHECKOUT" = "$EV_ROOT" ]; then
        EV_SPARE=1
        EV_WHY="the checkout --evict runs from gives up only a rung-6 incremental dir, in mode current"
        return 1
      fi ;;
    tmp:0:tree|tmp:3:tree) ;;
    leftover:0:tree) ;;
    main:6:incremental) ;;
    lane:6:incremental|fleet:6:incremental)
      if [ -n "$EV_CHECKOUT" ] && [ "$EV_CHECKOUT" = "$EV_ROOT" ] && [ "$_m" = current ]; then return 0; fi
      EV_WHY="rung 6 reaches a lane's tree only as the current checkout's, in mode current"; return 1 ;;
    main:*) EV_WHY="the main checkout gives up only an incremental dir, at rung 6"; return 1 ;;
    *) EV_WHY="rung $_r $_s is not an eviction the ladder makes from a $_c tree"; return 1 ;;
  esac
  return 0
}

# Has this lane's worktree (and, for a fleet tree, its lane dir under $PLX_FLEET_DIR) changed within
# $1 minutes? Skips a directory this run has itself already taken something from.
ev_lane_active() {
  _m=$1
  if [ -n "$EV_CHECKOUT" ]; then
    case "$EV_TOUCHED" in *"|$EV_CHECKOUT|"*) ;; *) idle_lane "$EV_CHECKOUT" "$_m" || return 0 ;; esac
  fi
  if [ "$EV_CLASS" = fleet ]; then
    _lanedir=$(dirname "$EV_TREE")
    case "$EV_TOUCHED" in *"|$_lanedir|"*) ;; *) idle_lane "$_lanedir" "$_m" || return 0 ;; esac
  fi
  return 1
}

# One `evict` line: rung scope est_kib owner idle_min mode path.
ev_line() {
  _rung=$1 _scope=$2 _est=$3 _owner=$4 _idle=$5 _mode=$6 _path=$7
  case "$_rung" in [0-6]) ;; *) ev_refuse "$_path" "bad rung '$_rung'"; return 0 ;; esac
  case "$_scope" in tree|incremental) ;; *) ev_refuse "$_path" "bad scope '$_scope'"; return 0 ;; esac
  case "$_est" in ''|*[!0-9]*) ev_refuse "$_path" "bad estimate '$_est'"; return 0 ;; esac
  case "$_idle" in ''|*[!0-9]*) ev_refuse "$_path" "bad idle minutes '$_idle'"; return 0 ;; esac
  case "$_mode" in normal|current) ;; *) ev_refuse "$_path" "bad mode '$_mode'"; return 0 ;; esac
  case "$_path" in /*) ;; *) ev_refuse "$_path" "not an absolute path"; return 0 ;; esac
  if [ -L "$_path" ]; then ev_refuse "$_path" "a symlink"; return 0; fi
  if [ ! -e "$_path" ]; then printf '  already gone     %s\n' "$_path"; return 0; fi
  if [ ! -d "$_path" ]; then ev_refuse "$_path" "not a directory"; return 0; fi
  _real=$(ev_phys "$_path") || { ev_refuse "$_path" "cannot be resolved"; return 0; }
  if ! ev_classify "$_real"; then
    ev_refuse "$_path" "not a derived PlxNative build tree: not under a checkout's rust-modules/target*, the fleet dir, the cache's leftovers or a /tmp cargo tree of ours"
    return 0
  fi
  case "$_scope" in
    tree) if [ "$_real" != "$EV_TREE" ]; then ev_refuse "$_path" "scope tree, but the path is inside a tree rather than the tree itself"; return 0; fi ;;
    incremental)
      if [ "$_real" = "$EV_TREE" ] || [ "${_real##*/}" != incremental ]; then
        ev_refuse "$_path" "scope incremental, but the path is not an incremental dir inside a tree"; return 0
      fi ;;
  esac
  if ! ev_allowed "$EV_CLASS" "$_rung" "$_scope" "$_mode" "$_real"; then
    if [ -n "$EV_SPARE" ]; then ev_skip "$_path" "spared: $EV_WHY"; else ev_refuse "$_path" "$EV_WHY"; fi
    return 0
  fi

  # --- liveness, re-asked now ---
  _cur=0
  if [ "$_mode" = current ] && [ "$_rung" = 6 ] && [ "$EV_CHECKOUT" = "$EV_ROOT" ]; then _cur=1; fi
  if live_checkouts >/dev/null 2>&1; then
    :
  elif _b=$(builders_running); then
    ev_skip "$_path" "$_b, and this host cannot say which checkout it is building in"
    return 0
  fi
  if [ "$_cur" = 0 ] && in_live_checkout "$_real" 2>/dev/null; then
    ev_skip "$_path" "a build is live in its checkout"
    return 0
  fi
  if [ "$_idle" -gt 0 ] && { [ "$EV_CLASS" = lane ] || [ "$EV_CLASS" = fleet ]; } && [ "$EV_CHECKOUT" != "$EV_MAIN" ]; then
    if ev_lane_active "$_idle"; then
      ev_skip "$_path" "its lane changed within $_idle min"
      return 0
    fi
  fi
  set -- --root "$ROOT" --tree "$EV_TREE" --class "$EV_CLASS" --min-age-secs $((_idle * 60))
  if [ -n "$EV_CHECKOUT" ]; then set -- "$@" --checkout "$EV_CHECKOUT"; fi
  if [ -n "$EV_LANE" ]; then set -- "$@" --lane "$EV_LANE"; fi
  if [ "$_cur" = 1 ]; then set -- "$@" --current; fi
  if ! _why=$(ev_py probe "$@" 2>&1); then
    case "$_why" in
      *"git tracks"*) ev_refuse "$_path" "$(printf '%s' "$_why" | head -n 1)" ;;
      *) ev_skip "$_path" "$(printf '%s' "$_why" | head -n 1)" ;;
    esac
    return 0
  fi

  # --- the eviction ---
  EV_N=$((EV_N + 1))
  _sz=$(fmt_kb "$_est")
  _label="(rung $_rung $_scope, from $_owner)"
  if [ -n "$DRY" ]; then
    printf '  would remove %8s  %s  %s\n' "~$_sz" "$_real" "$_label"
    EV_DONE=$((EV_DONE + 1))
    EV_FREED_KIB=$((EV_FREED_KIB + _est))
    return 0
  fi
  case "$_real" in
    *.tombstone.*) _tomb=$_real ;;
    *) _tomb="$EV_TREE.tombstone.$$.$EV_N" ;;
  esac
  if [ "$_tomb" != "$_real" ] && ! mv "$_real" "$_tomb" 2>/dev/null; then
    printf '  FAILED to rename %s — left in place\n' "$_real" >&2
    EV_SKIPPED=$((EV_SKIPPED + 1))
    return 0
  fi
  if ! rm -rf "$_tomb" 2>/dev/null; then
    printf '  could not finish removing %s — a later sweep will find it\n' "$_tomb" >&2
  fi
  printf '  removed      %8s  %s  %s\n' "~$_sz" "$_real" "$_label"
  gc_log "evict: rung $_rung $_scope freed ~$_sz from $_owner: $_real"
  EV_DONE=$((EV_DONE + 1))
  EV_FREED_KIB=$((EV_FREED_KIB + _est))
  if [ -n "$EV_CHECKOUT" ]; then EV_TOUCHED="$EV_TOUCHED$EV_CHECKOUT|"; fi
  if [ "$EV_CLASS" = fleet ]; then EV_TOUCHED="$EV_TOUCHED$(dirname "$EV_TREE")|"; fi
  return 0
}

run_evict_mode() {
  if [ ! -f "$ROOT/tools/disk-budget.py" ] || ! command -v python3 >/dev/null 2>&1; then
    echo "build-gc: --evict re-probes every path with tools/disk-budget.py and python3; one is missing here — refusing." >&2
    EV_EXIT=1
    return 0
  fi
  if [ ! -f "$PLAN_FILE" ]; then
    echo "build-gc: no plan file at $PLAN_FILE" >&2
    EV_EXIT=2
    return 0
  fi
  # A plan this script was not written for is not read at all: the first line names the format.
  if [ "$(grep -m 1 -v '^[[:space:]]*$' "$PLAN_FILE" 2>/dev/null)" != "# plx-disk-plan v1" ]; then
    echo "build-gc: $PLAN_FILE is not a 'plx-disk-plan v1' plan — refusing to read it." >&2
    EV_EXIT=2
    return 0
  fi
  trap ev_cleanup EXIT
  EV_MAIN=$(ev_phys "$MAIN") || EV_MAIN=$MAIN
  EV_ROOT=$(ev_phys "$ROOT") || EV_ROOT=$ROOT
  EV_WT_FILE="${TMPDIR:-/tmp}/plx-build-gc-evict-wt.$$"
  worktrees > "$EV_WT_FILE"
  _goal=$(sed -n 's/^# goal_kib //p' "$PLAN_FILE" | head -n 1)
  case "$_goal" in ''|*[!0-9]*) _goal= ;; esac
  echo "== evict: the planner's order, each path re-checked against the live machine first =="
  _tab=$(printf '\t')
  while IFS="$_tab" read -r _kind _k_rung _k_scope _k_est _k_owner _k_idle _k_mode _k_path <&3; do
    case "$_kind" in ''|'#'*) continue ;; esac
    if [ -n "$_goal" ] && [ -z "$DRY" ]; then
      _now=$(free_kib); _now=${_now:-0}
      if [ "$_now" -ge "$_goal" ] 2>/dev/null; then
        echo "evict: free space is back over the plan's goal ($(fmt_kb "$_now")) — the rest of the plan is not run"
        break
      fi
    fi
    case "$_kind" in
      evict) ev_line "$_k_rung" "$_k_scope" "$_k_est" "$_k_owner" "$_k_idle" "$_k_mode" "$_k_path" ;;
      stale)
        if [ "$_k_rung" != 5 ]; then ev_refuse "$_k_path" "the --stale sweep is a rung-5 step, not rung $_k_rung"; continue; fi
        echo "== evict: the --stale sweep, at its rung-5 slot =="
        _out=$(sh "$SELF" --stale ${DRY:+-n} 2>&1 </dev/null) || true
        printf '%s\n' "$_out"
        _n=$(printf '%s\n' "$_out" | grep -cE '  removed|would remove' || true)
        EV_DONE=$((EV_DONE + _n))
        gc_log "evict: --stale sweep at rung 5 (${_n} hash group(s))"
        ;;
      *) ev_refuse "$_k_path" "unknown plan line '$_kind'" ;;
    esac
  done 3< "$PLAN_FILE"
  if [ -n "$DRY" ]; then _verb="would be removed"; else _verb=removed; fi
  echo "evict: $EV_DONE $_verb (~$(fmt_kb "$EV_FREED_KIB") by estimate), $EV_SKIPPED skipped, $EV_REFUSED refused"
  if [ "$EV_REFUSED" -gt 0 ]; then EV_EXIT=1; fi
  if [ -z "$DRY" ]; then
    _after=$(free_kib); _after=${_after:-0}
    if _settle=$(ev_py settle --plan "$PLAN_FILE" --free-kib "$_after" 2>&1); then
      echo "settle: $_settle"
    else
      echo "settle: $_settle"
      if [ "$EV_EXIT" = 0 ]; then EV_EXIT=3; fi
    fi
  fi
  return 0
}

# `--auto` is fully self-contained (it shells back out to this same script per stage, each of
# which runs its own preflight) and exits here, BEFORE the ordinary preflight below — the lock
# gate inside `run_auto_mode` has to be the very first thing a second concurrent `--auto` hits,
# not something reached only after it has already enumerated worktrees and taken the FFmpeg lock.
if [ "$MODE" = auto ]; then
  run_auto_mode
  exit $?
fi

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
  #
  # `--evict` is the one mode that does NOT take it. It deletes derived cargo trees only and never a
  # live entry of the FFmpeg cache (bar an interrupted delete's tombstone; it does remove day-old
  # cargo-seed leftovers), so an FFmpeg build writing into the cache is no reason to leave a full disk
  # full: an FFmpeg build is exactly when disk is short.
  if [ "$MODE" != evict ] && [ -z "$DRY" ] && busy=$(ffmpeg_lock_held); then
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
  elif [ -z "$DRY" ] && { if [ "$MODE" = evict ]; then busy=$(builders_running); else busy=$(build_is_running); fi; }; then
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
    echo "exported CARGO_TARGET_DIR / SIM_TDIR (listed, not targeted: only a PlxNative cargo tree under /tmp or \$TMPDIR that --evict finds idle is ever deleted):"
    printf '%s\n' "$envx" | while IFS= read -r d; do
      printf '  %8s  %s\n' "$(fmt_kb "$(du -sk "$d" 2>/dev/null | awk '{print $1}')")" "$d"
    done
  fi

  cache=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
  echo
  if [ -n "$cache" ] && [ -d "$cache" ]; then
    printf 'shared build cache  %8s  %s (FFmpeg: one copy per configuration, for every checkout)\n' \
           "$(fmt_kb "$(du -sk "$cache" 2>/dev/null | awk '{print $1}')")" "$cache"
  fi
  if [ -n "$cache" ] && [ -d "$cache/cargo-seed" ]; then
    # `du` counts a clone's blocks in full, in the seed AND in every lane that was seeded from it,
    # so the lane totals above and this line overstate what is on the disk: the volume line below
    # (`df`) is the truth, and it is what `--auto` acts on.
    printf '  of which cargo seed %7s  %s (APFS clones of third-party output; du counts shared bytes in full, df is the truth)\n' \
           "$(fmt_kb "$(du -sk "$cache/cargo-seed" 2>/dev/null | awk '{print $1}')")" "$cache/cargo-seed"
  fi
  df -h "$ROOT" | tail -1 | awk '{print "volume              " $4 " free of " $2}'
  # The disk budget's verdicts (tools/disk-budget.py, docs/disk-budget.md): the private bytes each
  # tree would really free (`du` above counts APFS clones in full), the PlxNative cargo trees under
  # /tmp that nothing above can see, and the order a reclaim WOULD take them in. Read-only. It is
  # run, not sourced, and a failure of it must not end the report: this script is `set -eu`, and
  # the table above is complete without it. The existence test is for the synthetic repositories
  # ci/test_build_gc.py builds, which copy this script alone.
  if [ -f "$ROOT/tools/disk-budget.py" ] && command -v python3 >/dev/null 2>&1; then
    echo
    python3 "$ROOT/tools/disk-budget.py" report --root "$ROOT" \
      || echo "  (tools/disk-budget.py report failed; the table above stands on its own)"
  fi
  echo
  echo "reclaim with: tools/build-gc.sh --orphans | --incremental | --stale | --cache | --seed | --lanes | --worktrees | --all | --evict PLAN   (add -n to preview)"
  echo "the planner's own order: python3 tools/disk-budget.py reclaim --to GIB --emit > plan; tools/build-gc.sh --evict plan -n"
  ;;
esac

case "$MODE" in
evict)
  run_evict_mode
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

case "$MODE" in
stale|all)
  echo "== stale per-hash cargo artifacts in lanes and external fleet trees (kept: newest hash per crate/kind, plus anything under ${STALE_HOURS}h old) =="
  { worktrees | while IFS= read -r w; do
      [ "$w" = "$MAIN" ] && continue
      target_roots "$w" | while IFS= read -r t; do stale_deps_dirs "$t"; done
    done
    if [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ]; then
      for lane_dir in "$FLEET_DIR"/*; do
        [ -d "$lane_dir" ] || continue
        external_target_roots "$lane_dir" | while IFS= read -r t; do stale_deps_dirs "$t"; done
      done
    fi
  } | sort -u | while IFS= read -r dd; do
      [ -n "$dd" ] || continue
      if in_live_checkout "$dd" 2>/dev/null; then
        printf '  in use, skipped  %s\n' "$dd" >&2
        continue
      fi
      run_stale_sweep_dir "$dd" kind
    done
  echo "== stale per-hash cargo artifacts in the MAIN checkout (age only, never the newest hash per crate) =="
  target_roots "$MAIN" | while IFS= read -r t; do stale_deps_dirs "$t"; done | sort -u | while IFS= read -r dd; do
      [ -n "$dd" ] || continue
      if in_live_checkout "$dd" 2>/dev/null; then
        printf '  in use, skipped  %s\n' "$dd" >&2
        continue
      fi
      run_stale_sweep_dir "$dd" crate
    done
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
    # machine — rebuilt into and copied out of daily, all of it below `ffmpeg-<version>/` — would keep
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
      # and interrupting it leaves the extracted `ffmpeg-<version>` tree PRESENT AND PARTIAL — which is precisely the
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
  # The cargo seed (`tools/cargo-seed.py`) lives beside the FFmpeg trees but follows its own
  # protocol: the script takes the seed's flock, so a restore or harvest in flight keeps its seed.
  # Absent in the synthetic repositories ci/test_build_gc.py builds, hence the existence test.
  if [ -f "$ROOT/tools/cargo-seed.py" ]; then
    python3 "$ROOT/tools/cargo-seed.py" prune --days "$CACHE_MAX_DAYS" ${DRY:+--dry-run}
  fi
  ;;
esac

SEED_MAX_DAYS=${PLX_SEED_MAX_DAYS-7}
case "$MODE" in
seed)
  echo "== the cargo seed, unused for over $SEED_MAX_DAYS days =="
  if [ -f "$ROOT/tools/cargo-seed.py" ]; then
    python3 "$ROOT/tools/cargo-seed.py" prune --days "$SEED_MAX_DAYS" ${DRY:+--dry-run}
  fi
  ;;
esac

case "$MODE" in
worktrees)
  echo "== finished linked worktrees (clean, unlocked, already on origin/main (the local main without an origin); main and the running checkout are never touched) =="
  worktree_records | while IFS="$(printf '\t')" read -r w sha branch locked; do
    [ -n "$w" ] || continue
    reason=$(worktree_reason "$w" "$sha" "$locked")
    if [ -n "$reason" ]; then
      printf '  %-9s %s  (%s)\n' "$reason" "$w" "$branch"
      continue
    fi
    # What removing it would take that `make` cannot rebuild (see `private_ignored_paths`): read BEFORE the
    # removal, since afterwards there is nothing to read. Only a lane that is otherwise removable gets here,
    # so the probe is one `git status` per finished lane, never one per lane.
    _priv_ok=1
    _priv=$(private_ignored_paths "$w") || _priv_ok=
    # Under `--auto` only (PLX_GC_AUTO_IDLE_MIN is set by `run_auto_mode`'s call into this mode, never
    # by a human typing `--worktrees`; it is this mode's "nobody is watching" marker): a lane that LOOKS
    # finished is not necessarily nobody's. A
    # squash can land while an agent is still in the worktree reading it or about to branch from it,
    # and removing it then costs that agent its checkout. The same window `--evict` applies to another
    # lane's tree (`ev_lane_active`), over the worktree AND the lane's external build tree.
    # And nobody is watching what goes with it: a lane holding an ignored file that is not build output
    # (a private token, a lab session, a capture, notes) is kept, with the paths that kept it, and so is a
    # lane whose ignored files git could not list. By hand the human is shown them instead (below).
    if [ -n "${PLX_GC_AUTO_IDLE_MIN-}" ]; then
      _lane_active=
      if ! idle_lane "$w" "$PLX_GC_AUTO_IDLE_MIN"; then _lane_active=1; fi
      if [ -z "$_lane_active" ] && [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR/$(basename "$w")" ] \
         && ! idle_lane "$FLEET_DIR/$(basename "$w")" "$PLX_GC_AUTO_IDLE_MIN"; then
        _lane_active=1
      fi
      if [ -n "$_lane_active" ]; then
        printf '  recently active, skipped  %s  (%s)\n' "$w" "$branch" >&2
        continue
      fi
      if [ -z "$_priv_ok" ] || [ -n "$_priv" ]; then
        printf '  %-9s %s  (%s)\n' private "$w" "$branch"
        if [ -z "$_priv_ok" ]; then
          _why="git could not list its ignored files"
          printf '      %s\n' "$_why"
        else
          _why="holds ignored files that are not build output: $(join_paths "$_priv")"
          print_paths "      ignored and not build output, so it stays:" "$_priv"
        fi
        if [ -z "$DRY" ]; then gc_log "worktrees: kept $w ($branch): $_why"; fi
        continue
      fi
    fi
    if [ -n "$DRY" ]; then
      printf '  would remove  %s  (%s)\n' "$w" "$branch"
      print_private_note "      ignored and not rebuildable, goes with it:"
      continue
    fi
    # `worktree_reason` answered "building?" from the snapshot taken once before this whole loop
    # started (`live_checkouts`, above the mode dispatch) — a build that started in `$w` AFTER
    # that snapshot and before we reach it here is invisible to that answer. `live_checkouts` is
    # cheap (pgrep + lsof, not a `du`), so refresh it right before the one irreversible step
    # instead of trusting a snapshot that can be several worktrees stale. This narrows the race to
    # the moment between this refresh and `git worktree remove` itself — not zero, but far
    # smaller than the whole loop.
    live_checkouts >/dev/null 2>&1 || true
    if in_live_checkout "$w" 2>/dev/null; then
      printf '  in use, skipped  %s  (%s)\n' "$w" "$branch"
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
      print_private_note "      ignored and not rebuildable, went with it:"
      # Same staleness risk as above for the external tree, plus `$FLEET_DIR` itself: `${VAR-def}`
      # only substitutes when VAR is UNSET, so `PLX_FLEET_DIR=""` in the environment leaves
      # `FLEET_DIR` empty rather than defaulted, and `"$FLEET_DIR/$(basename "$w")"` would then be
      # a ROOT-level path like `/agent-abc`. Guard exactly like `external_trees()` does — non-empty
      # AND an existing directory — before ever building that path.
      if [ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ]; then
        ext="$FLEET_DIR/$(basename "$w")"
        live_checkouts >/dev/null 2>&1 || true
        if [ -d "$ext" ]; then
          if in_live_checkout "$ext" 2>/dev/null; then
            printf '    in use, skipped  %s\n' "$ext"
          else
            printf '%s\n' "$ext" | drop | sed 's/^/  /'
          fi
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
  # PLX_GC_AUTO_IDLE_MIN is set by `run_auto_mode` only around its `--worktrees` stage, and `--auto`
  # no longer calls `--lanes` at all (the planner's `--evict` took its place), so this idle guard is
  # DORMANT unless a human exports the variable by hand. A manual reclaim is an explicit ask and
  # idleness is not this mode's business; the guard is kept for a caller that does want it.
  worktrees | while IFS= read -r w; do
    [ "$w" = "$MAIN" ] && continue
    if [ -n "${PLX_GC_AUTO_IDLE_MIN-}" ] && ! idle_lane "$w" "$PLX_GC_AUTO_IDLE_MIN"; then
      printf '  recently active, skipped  %s\n' "$w" >&2
      continue
    fi
    lane_trees "$w"
  done | skip_live | drop
  echo "== external lane build trees =="
  if [ -n "${PLX_GC_AUTO_IDLE_MIN-}" ]; then
    external_trees | sort -u | while IFS= read -r d; do
      _lane=$(basename "$(dirname "$d")")
      _w=$(worktrees | while IFS= read -r ww; do
        [ "$(basename "$ww")" = "$_lane" ] && { echo "$ww"; break; }
      done)
      # Check BOTH: the worktree source tree (edits, checkouts) AND the external tree itself
      # (`$d`) — a lane's own `cargo build` writes into `$d`, not into the worktree, so a worktree
      # that has sat untouched for the idle window can still be mid-build if the target dir under
      # `$PLX_FLEET_DIR` is fresh. Either one being recently active is enough to spare the lane.
      if { [ -n "$_w" ] && ! idle_lane "$_w" "$PLX_GC_AUTO_IDLE_MIN"; } \
         || ! idle_lane "$d" "$PLX_GC_AUTO_IDLE_MIN"; then
        printf '  recently active, skipped  %s\n' "$d" >&2
        continue
      fi
      echo "$d"
    done | skip_live | drop
  else
    external_trees | sort -u | skip_live | drop
  fi
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
exit "$EV_EXIT"
