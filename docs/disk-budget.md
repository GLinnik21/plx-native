# The disk budget

Build output is bounded by the tooling, not by briefing every agent to clean up after itself. This
page is the design record of that tooling and the reference for the pieces that exist today:

- `tools/disk-budget.py`: the inventory, the planner (a pure function that decides WHAT to evict and
  in what order), the read-only `report` that `make disk` prints, `reclaim --emit` (the planner's
  decision as a file) and the probes the executor asks.
- `tools/build-gc.sh --evict PLAN`: the one narrow mode that deletes what a plan names, after asking
  the live machine about every path again. `build-gc.sh --auto` is the planner followed by it.
- `tools/disk-budget.py preflight`: what `make disk-preflight` runs at every build entry. With room
  it is one `statvfs`; without, it is the planner followed by `--evict`, then a re-measure.

The planner never deletes and the executor never decides: a plan is a proposal, and the executor
treats every line of it as untrusted.

Paths below use `~` for the home directory and `<repo>` for a checkout.

## Why

A build used to check for room at two targets and make none. That check, `make disk-preflight`
(`PLX_MIN_FREE_MB`, default 3072), only refused: it freed nothing, and only `check` and `check-replay`
depended on it. The cleanup that exists, `tools/build-gc.sh --auto`, runs on a clock (hourly from the
launchd agent, and at the end of a session), so between two ticks a fleet of lanes writes 3-8 GB
each. And the cargo target directories agents leave outside any checkout (the hostsim recipe in
`AGENTS.md` points `CARGO_TARGET_DIR` at `/tmp`) were visible to no tool: `build-gc.sh` deleted only
under `$PLX_FLEET_DIR` and merely reported an exported `CARGO_TARGET_DIR`. The planner now finds them,
and `--evict` takes the ones nobody is using.

Measured on the owner's machine, 2026-10-19 (these are point-in-time numbers, not constants):

- The volume was 228 GiB; the build-gc log shows free space at 8.3 GiB that morning and 0.9 GiB the
  evening before.
- `/tmp` held **45.6 GiB in 30 cargo trees by `du`**; 28 were PlxNative's, most with nothing written
  for more than a day, and two (10.4 GiB, an `rs-matter` evaluation) belonged to another project.
- **Of 5.5 GB per lane, 1.0 GB is shareable by content hash and 3.9 GB is per-lane by nature**
  (the crates that branch dirtied, plus workspace crates whose bytes embed the checkout path). The
  remaining ~0.6 GB is third-party output with no twin at that moment. Sharing by hash is real but
  small, so it is one rung of the ladder (rung 1, a later PR), not the mechanism.

## Reading `make disk`

`make disk` runs `tools/build-gc.sh` with no mode: its per-checkout table, and then the disk budget
report (`python3 tools/disk-budget.py report`, which can also be run on its own; `--root DIR`,
`--tree DIR` and `--need-gib N` choose the current checkout, the tree the next build would write and
its estimate). It prints, in order:

1. **The budget**: free space, the comfort line, the reserve, the next build's estimate, the ref lanes
   are judged against, and the **verdict** (`ok`, `would reclaim`, or `WOULD REFUSE` with the
   reason and the biggest trees that are live).
2. **Trees**: every derived tree found, with `PRIVATE` (what deleting it frees), `DU` (what `du`
   says), when cargo last wrote in it, and its state (the rungs it can be taken at, or why it is kept).
3. **The ladder**: every eviction a reclaim could take, in the order it would take them. A `*` marks
   what it would take at the free space shown; the running total shows where the line is crossed.
   The ladder always prints; set `PLX_GC_TEST_FREE_KIB` (the KiB override `build-gc.sh` already has)
   to see which of its rungs a reclaim would take, and the verdict, on a machine that has room:
   `PLX_GC_TEST_FREE_KIB=$((8*1024*1024)) python3 tools/disk-budget.py report`.

`PRIVATE` is the number to trust. On APFS a clone shares its blocks, so `du` counts them in full in
every tree that holds them and deleting one of a pair frees nothing. `PRIVATE` is the file's own
blocks (`getattrlist` `ATTR_CMNEXT_PRIVATESIZE`; allocated blocks where that cannot be asked, so a
Linux host over-reports clones rather than failing), summed per inode, and an inode counts only if
**every** one of its names lies inside what would be deleted. That matters because rustc hard-links
its incremental work products into `deps/`: removing `incremental/` alone does not free them, and a
freed-bytes estimate that said it did would stop the ladder early.

## The thresholds

| Setting | Value | Why |
|---|---|---|
| Comfort line | 20 GiB free (`PLX_GC_MIN_FREE_GIB`) | The line `build-gc.sh --auto` already uses: about an hour of fleet growth |
| Hysteresis | +4 GiB | A reclaim runs up to comfort + 4, about one cold host `target`, so the next build does not reclaim again |
| Reserve | 3 GiB (`PLX_MIN_FREE_MB`) | The margin kept on top of the build's estimate: the preflight refuses when free space cannot cover estimate + reserve. The bare reserve is the floor only when the preflight itself fails (it then fails open unless free space is below the reserve); it is also the figure the `df`-only check it replaced used |
| Idle | 15 min | A tree written in the last 15 minutes is somebody's current work; a lane is idle only when all of its trees are |
| `/tmp` garbage | 24 h | A PlxNative `/tmp` tree untouched for a day is nobody's current work |
| Seed | never planned | Lanes cloned from it share its blocks, so deleting it frees almost nothing (12 KiB private against 478 MB by `du`, measured) and makes every new lane's first build cold. `build-gc.sh --seed` (7 days, `PLX_SEED_MAX_DAYS`) stays as a manual prune |

The build's estimate is what it is expected to **add**: 3.4 GiB for `target`, 2.7 for `target-fast`,
3.5 for `target-hostsim`, 0.9 for `target-sim`, 0.15 for `target-storage`, 0.45 for `target-release`,
minus what the tree already holds, never below 0.5 GiB. `PLX_DISK_NEED_GIB` overrides it.

## The ladder

Least recently built first within each rung. A reclaim starts below the comfort line, runs rungs 0-5
until free space is back over comfort + hysteresis, and uses rung 6 only if the build would otherwise
be refused: free space still below the estimate plus the reserve.

| Rung | What | Cost to its owner |
|---|---|---|
| 0 | Garbage: tombstones, orphaned fleet trees, trees of lanes `origin/main` already contains, PlxNative `/tmp` trees untouched for 24 h | none, or nobody owns them |
| 1 | Content dedup of idle trees | none. **Not built yet; skipped** |
| 2 | Incremental dirs of OTHER idle lanes | that lane's next edit-rebuild is cold once |
| 3 | Younger PlxNative `/tmp` trees, idle 15 min | that command's next run is cold |
| 4 | Whole derived trees of OTHER idle lanes | that lane's next check rebuilds the workspace; third-party comes back from the seed |
| 5 | The main checkout's `--stale` sweep (not sized) | as `build-gc.sh --stale` today |
| 6 | Budget-critical only: the main checkout's incremental dirs, then the CURRENT checkout's incremental dirs in trees other than the one being built | one cold incremental build |

A **refusal** is the planner saying the whole ladder cannot free the build's estimate plus the
reserve. The `--stale` sweep cannot be sized ahead of time, so it counts as ZERO in the verdict (an
earlier version counted it as "might free enough", which made a refusal unreachable whenever a sweep
was in the plan). A plan whose sized actions fall short is a `refuse` even with the sweep in it; the
sweep still stays in the plan at its rung-5 slot, `--evict` runs it there, re-reads free space and
`settle` judges the result: `ok` if the sweep made room, `REFUSE` if not. The sweep is planned off the
main checkout's tree, and only while that tree is not in use and the main checkout is not the current
one: from a lane, or with `--no-build` (which has no current checkout, and is how `--auto` runs the
planner), it is planned at rung 5; from the main checkout with a build in view (`report`, or `reclaim`
without `--no-build`) the current checkout is spared and no sweep is planned.

**The cargo seed is never planned, at any age.** Earlier text had it at rung 5 after a week unused.
That measured as a bad trade: the lanes cloned from it share its blocks, so deleting it frees
almost nothing, and the next lane's first build is cold.

## What is never planned

- A tree that holds a tracked file.
- A live tree: a builder (`cargo`, `rustc`, `make`, `cc1`, the ARM `gcc`) with its working directory
  in the tree's checkout, any open file under the tree, a held cargo build lock, a held per-checkout
  `make check` lock, or an `lsof` that is missing, fails or prints nothing (ignorance counts as live).
- The current checkout, except rung 6, and there only trees other than the one being built, and only
  when no other process holds a file or a cargo lock in them. `make` itself runs in the current
  checkout, so for it the working-directory test cannot be used and the per-tree locks decide.
- The main checkout, except its `--stale` sweep (rung 5) and its incremental dirs (rung 6).
- The cargo seed (`<cache>/cargo-seed/<name>`), at any age.
- Anything under a checkout's `pkg/` or `vendor/` (the FFmpeg, libass and other builds: expensive,
  not derived from cargo) and the live entries of the shared FFmpeg cache (`<cache>/ffmpeg/<key>`).
  The one thing in that cache the planner takes, at rung 0, is a `.tombstone.` entry left by an
  interrupted delete, which nothing will ever read.
- A tree on another volume.
- **A cargo tree that is not PlxNative's.** `CACHEDIR.TAG` alone proves nothing (any cargo writes
  it); a tree is ours only if its `deps/`, `.fingerprint/`, `build/` or `incremental/` hold a
  `plx_*`, `libplx_*` or `plxnative*` entry. Other trees are reported as "not ours" and never planned.

The planner checks these again itself, on top of the inventory's filtering. It is a pure function
(`plan(inventory, free, need, thresholds, me, now)`; no I/O inside, tested with the filesystem,
subprocess and `open` made to raise), so `ci/test_disk_budget.py` feeds it synthetic inventories.

## How trees are found

- Every checkout's `rust-modules/target*`, from `git worktree list`.
- `$PLX_FLEET_DIR` (default `~/plx-fleet`): fleet lane trees, and the orphans whose worktree is gone.
- `/tmp` and `$TMPDIR` (or the `os.pathsep` list in `PLX_GC_TMP_ROOTS`): a directory carrying a
  `CACHEDIR.TAG` at most three levels below the root, found without entering a registered checkout
  or the fleet dir. `python3 tools/disk-budget.py tmp-trees` prints the PlxNative ones; it is the
  SAME scanner the report and the planner use, and `--evict` asks it again rather than carrying a
  second implementation.
- A `/tmp` tree's age is the later of its mtime and its **ctime**. An mtime can be set to anything
  (`os.utime`, `cp -p`, an archive restore, a test fixture); a ctime is the kernel's. A fixture
  whose files were backdated to last month is minutes old to the planner, so it is never "garbage
  nobody has written to for a day".
- `$PLX_BUILD_CACHE` (default `~/.cache/plxnative`): the cargo seed, and leftover `.tmp.`,
  `.restore.`, `.old.` and `.tombstone.` entries. The FFmpeg cache's live entries are never listed.

**Merged lanes are judged against `origin/main`** (by the report, the planner and `build-gc.sh --worktrees` alike) when it exists, because the local `main` can be days
behind it (then every squash-merged lane reads "unmerged"); with no `origin/main` the local `main` is
used, and the report's `lanes judged` line says which. A lane is merged if its HEAD is an ancestor, or if
a three-way `git merge-tree --write-tree` of the two leaves `origin/main`'s own tree. That command
writes the merged tree, so the report and `build-gc.sh --worktrees` (which `--auto` runs hourly) point it at a
throwaway object directory with the real one as an alternate: no object is added to the repository (git may refresh the mtime of an object that
already exists, which is what "already landed" looks like). Rung 0 additionally requires the lane to
be clean, not `git worktree lock`ed and idle. A lane with no commits yet also reads merged; that
costs it a rebuild at worst, and only its derived tree is ever planned, never the worktree.

**`build-gc.sh --worktrees` removes the worktree itself, and `git worktree remove` takes its IGNORED files
with it.** "Clean" does not see them: a lane can hold `src/config.local.h` (the real Plex token),
`tests/manifest.local.json`, `pkg/auth.json`, `pkg/lab.json`, `.tv-host`, `.tv-mac`, or its own notes and
captures, and none of that is rebuildable. Reaching squash-merged lanes made this fire unattended, so the
two ways of running the mode differ. **By hand** (a human typing `--worktrees`) it removes as before, and
`-n` and the run list under each lane every ignored path that is not build output, so the human sees what
goes with it. **Under `--auto`** (the hourly agent and the `SessionEnd` hook; the mode's own marker is
`PLX_GC_AUTO_IDLE_MIN`, which `--auto` sets around that one stage) a lane is removed only when EVERY ignored
path in it is build output: it is kept, reported with the reason `private` and the paths that kept it, and
the log line names them (`worktrees: kept <lane> (<branch>): holds ignored files that are not build
output: ...`). A lane whose ignored files git could not list is kept too (fail closed). Build output is
the allow-list `derived_output` in `tools/build-gc.sh`: `rust-modules/target*`, the FFmpeg, Sentry and libass
trees under `vendor/`, what `make` stages in `pkg/` (the binaries, `.so`s, `.link.*` evidence, `.flavor`,
`.build-config`), `ipkroot/`, `src/*.o`, `stub/*.so`, `tools/node_modules`, `__pycache__`, `.DS_Store`
and a TV session's `.tv-*.pid`/`.log` scratch. Everything else is kept: `pkg/*` is deliberately not a pattern,
and what was pulled from the television (`.abi-cache/`, `sysroot/`, `tests/fixtures/softfloat/arm.tbl`) is
kept because getting it back needs the one TV. A new `.gitignore` pattern must be added to that list or named as
private in `ci/test_build_gc.py`, which fails otherwise; until then a lane holding it is kept, never removed.

## Evicting a plan

```
python3 tools/disk-budget.py reclaim --to 20 --no-build --emit > plan    # decide
tools/build-gc.sh --evict plan -n                                         # preview, changes nothing
tools/build-gc.sh --evict plan                                            # do it
```

`reclaim --to GIB` is the planner for a reclaim up to `GIB` free (comfort line `GIB`, goal `GIB` + the
4 GiB hysteresis). `--no-build` means nothing is about to build (a timer or a hook): the estimate is 0
and no checkout is spared as "the current one". Without it, `--root` is the current checkout and
`--tree` / `--need-gib` name the build, as for `report`. `--emit` prints the plan as a file;
without it the same plan is printed for a person.

**The plan file** is `# plx-disk-plan v1`, `#` header lines (`verdict`, `free_kib`, `goal_kib`,
`wanted_kib`, `need_kib`, `reserve_kib`), then one tab-separated line per path, in the order to run
them:

```
evict  RUNG  SCOPE  EST_KIB  OWNER|-  IDLE_MIN  MODE  PATH        (SCOPE: tree | incremental; MODE: normal | current)
stale  5     stale  0        owner    0         normal  <main target>     (one line, at the sweep's slot)
```

`EST_KIB` is the action's private bytes and rides on its first path; `IDLE_MIN` is how long the
owner must still be idle when the executor looks again. A path with a tab or newline in its name is
left out, as a `# skipped` comment. A first line that is not `# plx-disk-plan v1` is not read at
all (exit 2).

**What `--evict` does with a line.** The plan is untrusted: a stale plan, a hand-edited one and one
from a future planner must all fail safe, so nothing is taken on its word.

1. *What it is.* The path is resolved physically and must be a derived PlxNative build tree, found
   again from the machine: a registered checkout's `rust-modules/target*` or an `incremental` dir in
   one; a `target*` under `$PLX_FLEET_DIR`; an interrupted delete's leftover (`.tmp.`, `.restore.`,
   `.old.`, `.tombstone.`) under `<cache>/cargo-seed` or a `.tombstone.` under `<cache>/ffmpeg`; or a
   tree `disk-budget.py tmp-trees` lists. Anything else is **refused** (reported as `REFUSED`, exit 1,
   nothing touched): a tracked file, `pkg/`, `vendor/`, another project's tree, a symlink, a path
   outside every root, the cargo seed, a live FFmpeg cache entry, a relative path, a bad rung or
   scope, and a path whose shape does not match its scope (`tree` is the tree root, `incremental` is
   an `incremental` dir inside one).
2. *What rung it may be.* The (kind of tree, rung, scope) must be an eviction the ladder makes: the
   main checkout gives up only an `incremental` dir at rung 6 and never its whole `target`; the
   checkout `--evict` runs from gives up only a rung-6 `incremental` in mode `current`; a leftover
   or tombstone only rung 0. A line that names the running checkout's own tree at rung 0, 2 or 4
   (a `reclaim --no-build` plan made from a lane can) is **skipped** as "spared", not refused: it
   is a fair eviction, just not from the checkout about to build.
3. *Whether it is live, asked again immediately before the rename.* A builder (`cargo`, `rustc`,
   `make`, `cc1`, the ARM `gcc`) with its working directory in the owning checkout, and
   `disk-budget.py probe`: a held cargo build lock, a held per-checkout `make check` lock, any open
   file under the tree, a tracked file (that one is a refusal, not a skip), a `/tmp` tree changed
   within the plan's idle window by the later of mtime and ctime, and a seed-run leftover younger
   than a day. For another lane's tree, also: nothing in the lane's worktree or its fleet build
   tree changed within `IDLE_MIN` minutes (not re-judged for a lane this run has already taken
   something from, whose directory mtimes the run itself just moved). An `lsof` that is missing,
   fails or prints nothing counts as live. A tree that turned live is **skipped** with the reason
   (`in use, skipped`), not refused: the plan was right when it was written.
4. *The deletion.* A rename to `<tree>.tombstone.<pid>.<n>`, a sibling of the tree root so that a
   later sweep finds an interrupted delete, then `rm -rf`. The log (`$PLX_GC_LOG`) gets one line per
   removal, `evict: rung R SCOPE freed ~SIZE from OWNER: PATH`.

After every path `--evict` re-reads free space and stops once it is over the plan's `goal_kib`.
It ends with `settle:`: whether the free space it re-read covers the plan's `need_kib` plus
`reserve_kib` (exit 3 when it does not; exit 1 if any path was refused, which wins). `-n` prints
`would remove` for what it would take and writes no log.

**Concurrency.** Every other `build-gc.sh` mode refuses outright while an FFmpeg cache lock is held,
because a build is writing into the shared cache. `--evict` does not: it never touches the FFmpeg
cache's live entries (bar a `.tombstone.` nothing reads), though it does remove day-old `cargo-seed`
leftovers (`.tmp.`, `.restore.`, `.old.`), and an FFmpeg build is exactly when the disk is likeliest to
be short. It does take the builder-in-checkout and per-tree locks above. The one exception to "ignores the FFmpeg
lock" is the `stale` line: it runs `build-gc.sh --stale` as a child, which keeps its own preflight and
declines (its message is printed in the output) while an FFmpeg build holds the cache lock; the sweep then
counts nothing and `settle` judges what the rest freed. It takes no lock of its own,
so two runs on one plan race benignly: the second finds the path gone or fails the rename and leaves
it.

**`build-gc.sh --auto`** is the planner followed by `--evict`. Always `--orphans` first, unconditionally.
Below `PLX_GC_MIN_FREE_GIB` free (default 20): `--worktrees` restricted to lanes idle for at least
`PLX_GC_IDLE_MIN` minutes (default 60, judged over the worktree and its fleet build tree) and holding no
ignored file that is not build output (see above: such a lane is kept and the log says which paths kept it), then, if still below, `reclaim --to $PLX_GC_MIN_FREE_GIB --no-build --idle-min
$PLX_GC_IDLE_MIN --emit` into a temporary file and `--evict` on it. It replaces five fixed stages
(`--incremental`, `--worktrees`, `--lanes`, `--stale`, `--seed`) with the planner's order, which
differs: the main checkout's incremental cache is the last resort (rung 6) instead of
the first stage; a lane's vendor build trees are not touched (they are not derived from cargo; only
`--lanes` and `--all` take them); the cargo seed is never evicted; PlxNative trees left under `/tmp`
are now in scope; and the stop line is the threshold plus 4 GiB. A planner that fails or is missing
deletes nothing and the log says so. The free-space gate, the single-instance lock and `-n` (which
takes no lock) are unchanged.

**Test seams.** `PLX_GC_TEST_FREE_KIB` (free space) and `PLX_GC_TEST_NOW` (the clock; honoured only
when `PLX_GC_TMP_ROOTS` is set) let a test pretend the disk is full or that time has passed.
`reclaim` refuses (exit 2, prints nothing) to plan under either while `PLX_GC_TMP_ROOTS`,
`PLX_FLEET_DIR` or `PLX_BUILD_CACHE` is left at its default: a pretend emergency over the owner's
real trees is how a test would delete them. `ci/test_build_gc.py` and `ci/test_disk_budget.py` point
all three at temp directories and never run `--evict` anywhere else.

## The preflight

`make disk-preflight` is `python3 tools/disk-budget.py preflight --tree <the dir the target builds
into>` and an ORDER-ONLY prerequisite (`$(DISK_ENTRIES): | disk-preflight`) of every Makefile target
that starts cargo. `make -s print-disk-entries` lists them. It is order-only because a phony normal
prerequisite would make every file target in the list out of date on every run; it still runs on a
`make` that has nothing to do, which is why the fast path matters. The decision, in order:

1. `PLX_DISK_BUDGET_DONE` set (and not `0`): exit 0. A recipe that re-enters make for a hooked target
   (`check` into `check-unlocked`, `install` into `deploy`, `build-bench`'s children) sets it on that
   line, so the child does not measure again. `PLX_MIN_FREE_MB=0` is exit 0 too.
2. **Fast path: one `statvfs`, exit 0** when free space is at least the comfort line and at least the
   target's UPPER estimate plus the reserve. This block sits before the module's heavy imports and
   needs only `os` and `sys`; a test pins that no subprocess, `tempfile`, `ctypes`, `argparse` or
   `dataclasses` is loaded on it. Measured 2026-10-19 over 20 runs of `python3 tools/disk-budget.py
   preflight`: median 21.3 ms against 14.2 ms for an empty interpreter (the earlier version, with
   every import first, took about 45 ms).
3. Below that, the full command. A tree on another volume than the checkout is judged on free space
   alone (nothing in the inventory is on its disk). Otherwise it takes `build-gc.sh`'s auto lock,
   plans `reclaim --to <comfort>` for the tree being built, sparing the CURRENT checkout (bar rung 6), runs
   `build-gc.sh --evict` on the plan, drops the inventory cache, re-reads free space and applies
   `settle`: exit 0 if the build plus the reserve now fits, exit 1 with the reason if not.
4. A plan that is a `refuse` and not `uncertain` evicts nothing (an eviction that cannot save the build
   is lost build time for nothing). One that is `uncertain` (the `--stale` sweep is in it) runs, because
   `settle` judges what it actually freed.

**What it will not do.**

- *Delete the tree being built, or anything of the checkout it runs from*, other than rung 6's
  incremental dirs in trees other than the one being built, and only when the build would otherwise
  be refused. `--evict` spares its own checkout by construction; the test runs it against a fixture
  fleet and asserts that the lane's tree, its incremental dirs and `target-sim` are all still there.
- *Wait on a lock for long.* If build-gc's auto lock (`${TMPDIR:-/tmp}/plx-build-gc-auto.lock`) is
  held it polls up to `--wait-secs` (default 60) and returns 0 the moment free space shows the build
  fits (it makes no plan while another reclaim runs, which is the one making the room). Each poll
  tries the lock again: a holder that has finished, or a dead holder's lock older than
  `PLX_GC_LOCK_STALE_MIN` (120), as `--auto` reclaims it, is taken over and the plan runs. At the
  deadline it judges on free space alone.
- *Fail a build for its own faults.* An exception in the preflight falls back to the bare floor: the
  build goes on unless free space is already below the reserve. A `build-gc.sh` that is missing,
  cannot be run or exceeds its 900 s timeout is judged like one that ran: `settle` on the free space
  re-read, so a build that fits still goes on.
- *Plan on a stale picture.* The inventory (the slow part, a walk of every tree) is cached under
  `$PLX_BUILD_CACHE/disk-budget/inventory-<hash of the roots>.json` for 10 minutes by file mtime,
  and dropped after any eviction. A cache that cannot be read, or that has another schema, is a miss.
  The executor re-probes every path whatever the cache says.
- *Run a pretend emergency over real trees.* With `PLX_GC_TEST_FREE_KIB` set it refuses (exit 2)
  while the roots are defaults, and, new with the preflight, when the current checkout is not
  under the named roots: a fixture's preflight cannot reach the owner's real fleet.

**Estimates.** The table value for the tree's directory name (`NEED_GIB` in `tools/disk-budget.py`)
unless the entry states its own: the ARM library is 0.6 GiB (`tvdev`) or 0.45 (`release`) though it
lives in a `target*` dir that a host build fills with 3.4 or more, the storage helper 0.15, and
`build-bench` 6.1 (the worst case: the host `target` plus a `test-fast` tree built from nothing, which only `ARGS=--cold` produces; a plain run reuses an existing `test-fast` tree or skips the `-inc` rows). `--need-gib N` and
`PLX_DISK_NEED_GIB` override it. The fast path compares against the UPPER estimate and the full path
subtracts what the tree already holds, so the fast path never says "go" where the planner would
say "reclaim".

**The managed `/tmp` replacements are entries too.** `make test-hostsim` builds into
`rust-modules/target-hostsim` (the table's 3.5 GiB, so `DISK_TREE` names that dir, not the lane's
`CARGO_TARGET_DIR`), and `make check-nodef` into the checkout's own `target` (or the
`CARGO_TARGET_DIR` a lane exports), where the table's 3.4 GiB is an upper bound: it adds only `check`
metadata beside `make lint`'s, so the planner subtracts what the tree already holds and a warm tree
needs the 0.5 GiB minimum. Both came from PR #845, which landed after this branch was cut; the static
test that finds every target whose recipe runs cargo is generic, and it failed on both until they
were added to `DISK_ENTRIES`.

## Not done, and not determined

- The hook that catches a direct `cargo`, and dedup, are the PRs below. A direct `cargo` in a lane
  reaches no preflight today.
- The `--stale` sweep's yield is not estimated. Sizing it (superseded `deps/` hashes) is open; until
  then it counts as zero in the verdict and `settle` judges the measured result.
- `--evict` asks `lsof` once per path (each probe takes its own snapshot, so a tree that turned live
  between two paths is seen). Measured 2026-10-19 on the owner's Mac with a busy fleet: 0.06-0.07 s
  and about 24 000 `-F` lines per snapshot, so 30 paths cost about two seconds. One snapshot per
  batch with a per-path re-check of only the cwd and cargo-lock signals would cut that and is not
  done; it would also make a tree that turned live mid-batch invisible, which is the reason for
  paying it.
- The cost of rung 4 and rung 6 evictions in lost build time across a real fleet day. Measure it
  from the build-gc log, which now records each eviction.
- Rung 1 saves ~0.8 GB per lane at most today, and nothing in a lane just cloned from the current
  seed.

## PR plan

| PR | Content |
|---|---|
| 1 (#846) | `tools/disk-budget.py` inventory, planner and `report`; `make disk` shows it; `ci/test_disk_budget.py`. No deletions |
| 2 (#848) | `build-gc.sh --evict`, `/tmp` discovery shared with the planner, `worktree_reason` against `origin/main` (and, because that makes `--auto` reach squash-merged lanes, `--worktrees` keeps a lane holding a non-build ignored file when unattended), `--auto` takes its order from the planner |
| 3 (this) | `disk-preflight` becomes the planner's preflight (`disk-budget.py preflight`), an order-only prerequisite of every cargo-invoking target on this branch; `print-disk-entries`; `ci/test_disk_preflight.py` |
| 4 | A `PreToolUse` hook that catches direct `cargo` in a Claude session |
| 5 (#845, landed) | `make test-hostsim` and `make check-nodef`, which replace the `/tmp` recipe; hooked into `DISK_ENTRIES` by PR 3 |
| 6 (optional) | `build-gc.sh --dedup` as rung 1 |
