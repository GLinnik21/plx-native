# The disk budget

Build output is bounded by the tooling, not by briefing every agent to clean up after itself. This
page is the design record of that tooling and the reference for its first piece, which exists today:
`tools/disk-budget.py`, the inventory, the planner and the read-only `report` that `make disk`
prints. **That piece deletes nothing.** Nothing acts on its plan yet; it is shipped first so the
verdicts can be read against real machines for a few days before anything is allowed to follow them.

Paths below use `~` for the home directory and `<repo>` for a checkout.

## Why

A build never checks for room and never makes any. The only build-time check, `make disk-preflight`
(`PLX_MIN_FREE_MB`, default 3072), refuses; it frees nothing, and only `check` and `check-replay`
depend on it. The cleanup that exists, `tools/build-gc.sh --auto`, runs on a clock (hourly from the
launchd agent, and at the end of a session), so between two ticks a fleet of lanes writes 3-8 GB
each. And the cargo target directories agents leave outside any checkout (the hostsim recipe in
`AGENTS.md` points `CARGO_TARGET_DIR` at `/tmp`) were visible to no tool: `build-gc.sh` deletes only
under `$PLX_FLEET_DIR` and merely reports an exported `CARGO_TARGET_DIR`.

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
| Reserve | 3 GiB (`PLX_MIN_FREE_MB`) | The existing preflight's ENOSPC floor |
| Idle | 15 min | A tree written in the last 15 minutes is somebody's current work; a lane is idle only when all of its trees are |
| `/tmp` garbage | 24 h | A PlxNative `/tmp` tree untouched for a day is nobody's current work |
| Seed | 7 days (`PLX_SEED_MAX_DAYS`) | `build-gc.sh --seed`'s age |

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
| 5 | The main checkout's `--stale` sweep (not sized; only when the report runs from another checkout), and the cargo seed once unused for a week | as `build-gc.sh` today |
| 6 | Budget-critical only: the main checkout's incremental dirs, then the CURRENT checkout's incremental dirs in trees other than the one being built | one cold incremental build |

A **refusal** is the planner saying the whole ladder cannot free the build's estimate plus the
reserve. The `--stale` sweep cannot be sized ahead of time, so a plan that reaches it (rungs 0-4
fell short) is never a refusal: it says so, and the step that acts on it re-reads free space after
the sweep. The sweep is planned only when the report runs from another checkout: from the main
checkout, the main `target` is the current checkout and only its rung-6 incremental dirs are
candidates.

## What is never planned

- A tree that holds a tracked file.
- A live tree: a builder (`cargo`, `rustc`, `make`, `cc1`, the ARM `gcc`) with its working directory
  in the tree's checkout, any open file under the tree, a held cargo build lock, a held per-checkout
  `make check` lock, or an `lsof` that is missing, fails or prints nothing (ignorance counts as live).
- The current checkout, except rung 6, and there only trees other than the one being built, and only
  when no other process holds a file or a cargo lock in them. `make` itself runs in the current
  checkout, so for it the working-directory test cannot be used and the per-tree locks decide.
- The main checkout, except its `--stale` sweep (rung 5) and its incremental dirs (rung 6).
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
  `CACHEDIR.TAG` at most three levels below the root, found without entering a registered checkout.
- `$PLX_BUILD_CACHE` (default `~/.cache/plxnative`): the cargo seed, and leftover `.tmp.`,
  `.restore.`, `.old.` and `.tombstone.` entries. The FFmpeg cache's live entries are never listed.

**Merged lanes are judged against `origin/main`** when it exists, because the local `main` can be days
behind it (then every squash-merged lane reads "unmerged"); with no `origin/main` the local `main` is
used, and the report's `lanes judged` line says which. A lane is merged if its HEAD is an ancestor, or if
a three-way `git merge-tree --write-tree` of the two leaves `origin/main`'s own tree. That command
writes the merged tree, so the report points it at a throwaway object directory with the real one as
an alternate: no object is added to the repository (git may refresh the mtime of an object that
already exists, which is what "already landed" looks like). Rung 0 additionally requires the lane to
be clean, not `git worktree lock`ed and idle. A lane with no commits yet also reads merged; that
costs it a rebuild at worst, and only its derived tree is ever planned, never the worktree.

## Not done in this PR, and not determined

- No deletion: `build-gc.sh --evict`, build-entry preflights, hooks, `make test-hostsim` and dedup
  are the PRs below.
- The `--stale` sweep's yield is not estimated, so a plan that reaches it is "uncertain". Sizing it
  (superseded `deps/` hashes) is open.
- The cost of rung 4 and rung 6 evictions in lost build time across a real fleet day. Measure it
  from the build-gc log once evictions are logged.
- Rung 1 saves ~0.8 GB per lane at most today, and nothing in a lane just cloned from the current
  seed.

## PR plan

| PR | Content |
|---|---|
| 1 (this) | `tools/disk-budget.py` inventory, planner and `report`; `make disk` shows it; `ci/test_disk_budget.py`. No deletions |
| 2 | `build-gc.sh --evict` and `/tmp` discovery; `worktree_reason` against `origin/main`; `--auto` takes its order from the planner |
| 3 | `disk-preflight` becomes the planner's preflight, an order-only prerequisite of every cargo-invoking target |
| 4 | A `PreToolUse` hook that catches direct `cargo` in a Claude session |
| 5 | `make test-hostsim` and `make check-nodef`, which replace the `/tmp` recipe |
| 6 (optional) | `build-gc.sh --dedup` as rung 1 |
