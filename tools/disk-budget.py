#!/usr/bin/env python3
"""The disk budget: inventory of every derived build tree, and the planner that orders their eviction.

    disk-budget.py report    [--root DIR] [--tree DIR] [--need-gib N]
    disk-budget.py reclaim   --to GIB [--emit] [--no-build] [--idle-min N] [--root DIR] [--tree DIR] [--need-gib N]
    disk-budget.py tmp-trees [--root DIR]
    disk-budget.py probe     --tree DIR --class lane|fleet|tmp|leftover [...]

WHY THIS EXISTS. Nothing checks, or makes room, when a build starts: the only build-time check refuses
below 3 GiB for two targets, and the cleanup that exists runs on a clock (hourly, and at the end of a
session). Between two ticks a fleet of lanes writes 3-8 GB each, and the cargo trees agents leave in
`/tmp` were visible to no tool at all. `docs/disk-budget.md` is the design record: the measurements, the
ladder below, the safety rules and the PR plan. This file is the inventory, the pure planner and the
read-only questions asked of them; it deletes nothing and has no subcommand that could. The one deleting
path is `build-gc.sh --evict`, which takes the plan `reclaim --emit` prints and re-checks every path
against the live machine immediately before it renames it away. `make disk` prints `report` after
build-gc's own table; `build-gc.sh --auto` takes its order from `reclaim`.

THE PLANNER IS A PURE FUNCTION. `plan(inv, free, need, th, me, now, build_tree)` does no I/O: it is the one
place where order and safety are decided, and `ci/test_disk_budget.py` feeds it synthetic inventories.
The inventory is built separately (`collect`) by walking the disk, with every process, lock and git probe
behind `Probes` so a test can supply its own.

THE LADDER (docs/disk-budget.md; LRU, least recently built first, within each rung):

    0  garbage: tombstones, orphaned fleet trees, trees of lanes `origin/main` already contains,
       PlxNative `/tmp` trees untouched for 24 h
    1  content dedup of idle trees (a later PR; listed as skipped)
    2  incremental dirs of OTHER idle lanes (the main checkout waits for rung 6)
    3  younger PlxNative `/tmp` trees, idle >= 15 min
    4  whole derived trees of OTHER idle lanes
    5  the `--stale` sweep (not sized; planned unless the main checkout IS the current one: from a lane, or
       with `--no-build` as `--auto` runs it, it is planned; from the main checkout with a build in view it is not)
    6  budget-critical only: the main checkout's incremental dirs, then the CURRENT checkout's
       incremental dirs in trees other than the one being built

A reclaim starts below the comfort line (20 GiB free, `PLX_GC_MIN_FREE_GIB`; `reclaim --to` sets it), runs
rungs 0-5 until free space is back over comfort + 4 GiB, and touches rung 6 only if the build would otherwise
be refused, i.e. free space is still below the build's estimate plus the 3 GiB reserve (`PLX_MIN_FREE_MB`).

THE REFUSAL RULE. The `--stale` sweep cannot be sized ahead, so the verdict counts it as ZERO: when the sized
actions cannot reach need + reserve the verdict is `refuse`, whether or not the sweep is in the plan. The
sweep stays in the action list (`uncertain`), it runs in its rung-5 slot, and the caller decides on the free
space it RE-READS afterwards: `build-gc.sh --evict` prints that as `settle: ok|REFUSE`. A refusal is thus
reachable from every checkout, and a sweep that rescues the build is not wasted.

WHAT A DELETION FREES is the tree's PRIVATE bytes: on APFS a clone shares its blocks, `du` counts them in
full in every tree that holds them, and deleting one of a pair frees nothing. Bytes are summed per inode
(a hard link is one inode), and only inodes whose every name lies inside what is deleted count: rustc
hard-links its incremental work products into `deps/`, so removing `incremental/` alone does not free
them. Off macOS the private size falls back to allocated blocks.

NEVER PLANNED (`never`, checked again inside the planner, not only when the inventory is built): a tree
with a tracked file, a live tree (a builder's cwd in its checkout, an open file under it, a held cargo
lock, a held `make check` lock, or an `lsof` that could not answer), the current checkout (except rung 6,
and there never the tree being built), the main checkout (except its `--stale` sweep at rung 5 and its
incremental dirs at rung 6), anything under a checkout's `pkg/` or `vendor/` and the shared FFmpeg cache's
live entries (its `.tombstone.` leftovers are rung 0), a tree on another volume, any cargo tree without
a PlxNative marker (`CACHEDIR.TAG` alone proves nothing: two `rs-matter` trees belong to another project
and are reported "not ours"), and the cargo seed (its blocks are shared with the lanes cloned from it, so
deleting it frees almost nothing: measured 12K private against 478M by `du`).

A `/tmp` TREE'S AGE is the later of its mtime and its ctime. An mtime can be set to anything (a test
fixture, `cp -p`, a restore from an archive); a ctime cannot, so a backdated tree reads as freshly built
and is never "garbage that nobody wrote to for a day".

Environment: PLX_GC_MIN_FREE_GIB (comfort, default 20), PLX_MIN_FREE_MB (reserve, default 3072),
PLX_DISK_NEED_GIB (the build's estimate), PLX_SEED_MAX_DAYS (7; the `--seed` mode), PLX_FLEET_DIR (default
~/plx-fleet), PLX_BUILD_CACHE (default ~/.cache/plxnative), PLX_GC_TMP_ROOTS (os.pathsep list of directories
to look for `/tmp` trees in; default /tmp and $TMPDIR), and two TEST-ONLY overrides: PLX_GC_TEST_FREE_KIB
(free space, the name build-gc.sh already uses) and PLX_GC_TEST_NOW (the clock, honoured only when
PLX_GC_TMP_ROOTS is set). `reclaim --emit` REFUSES to run with either override while PLX_GC_TMP_ROOTS,
PLX_FLEET_DIR or PLX_BUILD_CACHE is left to its default: a pretend emergency over the real default roots
is how a test would delete the owner's real trees.
"""
import argparse
import ctypes
import fcntl
import importlib.util
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, replace
from typing import Callable, Dict, Iterable, List, Optional, Sequence, Tuple

GIB = 1 << 30
MIB = 1 << 20

# --- the thresholds, and where each comes from (docs/disk-budget.md, "The ladder") ------------------
DEFAULT_COMFORT_GIB = 20         # the line `build-gc.sh --auto` already uses: ~an hour of fleet growth
HYSTERESIS_GIB = 4               # about one cold host `target`, so the next build does not reclaim again
DEFAULT_RESERVE_MIB = 3072       # `make disk-preflight`'s PLX_MIN_FREE_MB: the ENOSPC floor
IDLE_SECS = 15 * 60              # a tree written in the last 15 minutes is somebody's current work
TMP_GARBAGE_SECS = 24 * 3600     # a PlxNative /tmp tree this old is nobody's current work
LEFTOVER_SECS = 24 * 3600        # cargo-seed's own LEFTOVER_MAX_AGE
MIN_NEED_GIB = 0.5

# What a build is expected to ADD to its tree, measured (docs/disk-budget.md). The need is this minus
# what the tree already holds, at least MIN_NEED_GIB. The ARM builds (0.6 GiB `tvdev`, 0.45 GiB
# `release`) share the `target` / `target-release` names with larger host builds, so a build entry that
# knows better passes its own number.
NEED_GIB = {
    "target": 3.4,               # host debug, `make check`
    "target-fast": 2.7,
    "target-hostsim": 3.5,
    "target-sim": 0.9,
    "target-storage": 0.15,
    "target-release": 0.45,
}
DEFAULT_NEED_GIB = 3.4

PLX_MARKERS = ("plx_", "libplx_", "plxnative")   # entry names that make a cargo tree ours
PROFILE_SUBDIRS = (".fingerprint", "deps", "build", "incremental")
BUILDER_COMMANDS = ("cargo", "rustc", "make", "cc1", "arm-webos-linux-gnueabi-gcc")  # build-gc.sh's list
TMP_DEPTH = 3                    # `find -maxdepth 4 -name CACHEDIR.TAG`: the tree is at most 3 below a root
LEFTOVER_MARKS = (".tmp.", ".restore.", ".old.", ".tombstone.")   # cargo-seed.py's own list
KINDS = ("lane", "main", "fleet", "tmp", "seed", "leftover")

SKIPPED_RUNGS = {1: "content dedup is a later PR; the rung is skipped until it lands"}
SEED_KEPT = ("the cargo seed shares its blocks with the lanes cloned from it: deleting it frees almost nothing "
             "(measured 12K private against 478M by du); `build-gc.sh --seed` is the manual, age-gated prune")


# --- records -----------------------------------------------------------------------------------------
@dataclass(frozen=True)
class Thresholds:
    comfort: int = DEFAULT_COMFORT_GIB * GIB
    hysteresis: int = HYSTERESIS_GIB * GIB
    reserve: int = DEFAULT_RESERVE_MIB * MIB
    idle_secs: int = IDLE_SECS
    tmp_garbage_secs: int = TMP_GARBAGE_SECS


@dataclass(frozen=True)
class Liveness:
    """Why a tree is in use. Separate signals, because rung 6 trusts only some of them for the current
    checkout: `make` itself runs there, so its builder cwd always reads "live" at build start."""
    builder_cwd: str = ""                 # command of a builder whose cwd is in the owning checkout
    open_files: Tuple[str, ...] = ()      # commands holding a file open under the tree
    cargo_lock: bool = False              # a cargo build lock in the tree is held
    check_lock: bool = False              # the checkout's per-worktree `make check` lock is held
    unknown: bool = False                 # `lsof` was missing or failed: ignorance counts as live

    def reasons(self, current: bool = False) -> List[str]:
        out = []
        if self.builder_cwd and not current:
            out.append("%s is running in the checkout" % self.builder_cwd)
        if self.open_files:
            out.append("open by " + ", ".join(self.open_files))
        if self.cargo_lock:
            out.append("cargo lock held")
        if self.check_lock and not current:
            out.append("`make check` running")
        if self.unknown:
            out.append("liveness unknown (no lsof)")
        return out


@dataclass(frozen=True)
class Tree:
    path: str                             # realpath
    kind: str                             # one of KINDS
    checkout: Optional[str] = None        # realpath of the owning checkout; None: orphan, /tmp, cache
    owner: str = ""                       # lane name, "main", or "" for a tree nobody owns
    tombstone: bool = False               # the name carries `.tombstone.`: an interrupted delete
    orphan: bool = False                  # a fleet tree whose worktree is gone
    plx: bool = True                      # a PlxNative marker was found in it
    tracked: bool = False                 # git tracks a file under it
    dev: int = 0
    private_bytes: int = 0                # what deleting the whole tree frees
    alloc_bytes: int = 0                  # `du`-style: allocated blocks, each inode once
    incr_paths: Tuple[str, ...] = ()      # the `incremental` dirs inside it
    incr_bytes: int = 0                   # what deleting exactly those frees
    last_built: float = 0.0               # epoch of the newest file cargo wrote
    live: Liveness = Liveness()
    merged: bool = False                  # the owning lane's HEAD is already contained in the base ref
    clean: bool = False                   # ... and its working tree has no change
    locked: bool = False                  # ... and git does not lock the worktree (a live session)
    protected: str = ""                   # non-empty: never plan it, and why (a test seam)


@dataclass(frozen=True)
class Inventory:
    trees: Tuple[Tree, ...]
    volume_dev: int = 0
    protected_prefixes: Tuple[str, ...] = ()
    cache_root: str = ""
    base_ref: str = ""
    checkouts: Tuple[Tuple[str, str], ...] = ()   # (path, name) for the report


@dataclass(frozen=True)
class Action:
    rung: int
    scope: str                            # "tree" | "incremental" | "stale"
    tree: str
    paths: Tuple[str, ...]
    frees: int
    sized: bool
    owner: str
    why: str
    last_built: float
    group: int = 0                        # rung 6 only: 0 = the main checkout, 1 = the current one
    idle_secs: int = 0                    # the executor must find the owner idle for this long, 0: no age gate


@dataclass(frozen=True)
class Plan:
    verdict: str                          # "ok" | "reclaim" | "refuse"
    actions: Tuple[Action, ...]
    free: int
    need: int
    reserve: int
    comfort: int
    goal: int                             # free space the reclaim runs up to (rungs 0-5)
    projected: int                        # free space after the chosen actions
    uncertain: bool                       # unsized actions (`--stale`) are in the plan
    reason: str
    biggest_live: Tuple[Tuple[str, int, str], ...] = ()


# --- the pure planner --------------------------------------------------------------------------------
def under(path: str, prefix: str) -> bool:
    return path == prefix or path.startswith(prefix.rstrip(os.sep) + os.sep)


def never(tree: Tree, inv: Inventory) -> str:
    """Why this tree can never be planned at any rung, or "". The inventory filters too; this is the
    second wall, and the tests feed it trees the inventory would never have produced."""
    if tree.protected:
        return tree.protected
    if tree.kind not in KINDS:
        return "unclassified (%s)" % tree.kind
    if tree.tracked:
        return "git tracks a file in it"
    if not tree.plx:
        return "not ours: no PlxNative marker"
    if inv.volume_dev and tree.dev and tree.dev != inv.volume_dev:
        return "on another volume than the build"
    if tree.kind == "seed":
        return SEED_KEPT
    if tree.kind == "leftover":
        if not inv.cache_root or not under(tree.path, inv.cache_root):
            return "not under the build cache"
        return ""
    for prefix in inv.protected_prefixes:
        if under(tree.path, prefix):
            return "inside %s (pkg, vendor and FFmpeg are not derived from cargo)" % prefix
    return ""


def classify(tree: Tree, inv: Inventory, th: Thresholds, me: Optional[str], now: float,
             build_tree: Optional[str], lane_built: Optional[Dict[str, float]] = None) -> Tuple[List[Action], str]:
    """The actions a tree can contribute, and (when it contributes none) why it is kept.

    `lane_built` maps a checkout to when anything in ITS trees was last built: a lane is idle only when all of
    its trees are, so a lane running `make test-hostsim` keeps its `target` too."""
    why = never(tree, inv)
    if why:
        return [], "never: " + why
    owner = tree.owner or "-"
    built = tree.last_built
    if tree.kind in ("lane", "fleet") and tree.checkout and lane_built:
        built = max(built, lane_built.get(tree.checkout, 0.0))
    idle = max(0.0, now - built) >= th.idle_secs
    handles = list(tree.live.reasons(current=True))
    handles = [r for r in handles if not r.startswith("cargo lock")]   # only open files / unknown

    def whole(rung: int, text: str, group: int = 0, idle_secs: int = 0) -> Action:
        return Action(rung, "tree", tree.path, (tree.path,), tree.private_bytes, True, owner, text,
                      tree.last_built, group, idle_secs)

    def incremental(rung: int, text: str, group: int = 0, idle_secs: int = 0) -> Action:
        return Action(rung, "incremental", tree.path, tree.incr_paths, tree.incr_bytes, True, owner, text,
                      tree.last_built, group, idle_secs)

    def worth(actions: List[Action], kept: str) -> Tuple[List[Action], str]:
        kept_actions = [a for a in actions if not a.sized or a.frees > 0]
        if actions and not kept_actions:
            return [], "shares all its blocks: deleting it frees nothing"
        return kept_actions, kept

    if tree.kind == "leftover":
        if tree.live.reasons(current=True):
            return [], "in use: " + "; ".join(tree.live.reasons(current=True))
        return worth([whole(0, "leftover of an interrupted delete or seed run")], "")
    if tree.tombstone:
        if handles:
            return [], "in use: " + "; ".join(handles)
        return worth([whole(0, "tombstone: an interrupted delete")], "")

    if me is not None and tree.checkout == me:
        if tree.path == build_tree:
            return [], "current checkout: the tree being built"
        reasons = tree.live.reasons(current=True)
        if reasons:
            return [], "current checkout; in use: " + "; ".join(reasons)
        if tree.incr_bytes <= 0:
            return [], "current checkout"
        acts, _ = worth([incremental(6, "current checkout's incremental dir in a tree it is not building",
                                     group=1)], "")
        return acts, "current checkout (rung 6 only)"

    reasons = tree.live.reasons()
    if reasons:
        return [], "in use: " + "; ".join(reasons)

    if tree.kind == "main":
        acts: List[Action] = []
        if tree.incr_bytes > 0:
            acts.append(incremental(6, "main checkout's incremental cache (budget-critical only)"))
        acts, _ = worth(acts, "")
        acts.append(Action(5, "stale", tree.path, (tree.path,), 0, False, owner,
                           "superseded per-hash artifacts, age-gated (size not estimated)", tree.last_built))
        return acts, "main checkout: its incremental cache waits for rung 6" if tree.incr_bytes > 0 else ""

    if tree.kind == "fleet" and tree.orphan:
        return worth([whole(0, "orphaned fleet tree: its worktree is gone")], "")
    if tree.kind in ("lane", "fleet") and tree.merged and tree.clean and not tree.locked and idle:
        return worth([whole(0, "its lane is already on %s" % (inv.base_ref or "main"), idle_secs=th.idle_secs)], "")
    if tree.kind == "tmp":
        if now - tree.last_built >= th.tmp_garbage_secs:
            return worth([whole(0, "PlxNative /tmp tree untouched for %s" % age_text(now - tree.last_built),
                                idle_secs=th.tmp_garbage_secs)], "")
        if not idle:
            return [], "built %s ago" % age_text(now - tree.last_built)
        return worth([whole(3, "PlxNative /tmp tree, idle %s" % age_text(now - tree.last_built),
                            idle_secs=th.idle_secs)], "")

    if not idle:
        return [], "%s %s ago (< %d min)" % ("built" if built == tree.last_built else "its lane built",
                                              age_text(now - built), th.idle_secs // 60)
    acts = []
    incr_taken = 0
    if tree.incr_bytes > 0:
        acts.append(incremental(2, "idle lane's incremental cache", idle_secs=th.idle_secs))
        incr_taken = tree.incr_bytes
    acts.append(replace(whole(4, "idle lane's derived tree", idle_secs=th.idle_secs),
                        frees=max(0, tree.private_bytes - incr_taken)))
    return worth(acts, "")


def ladder(inv: Inventory, th: Thresholds, me: Optional[str], now: float,
           build_tree: Optional[str] = None) -> Tuple[List[Action], Dict[str, str]]:
    """Every candidate in eviction order, and the reason each tree that contributes none is kept.
    Independent of free space: this is the order a reclaim WOULD take."""
    actions: List[Action] = []
    kept: Dict[str, str] = {}
    lane_built: Dict[str, float] = {}
    for tree in inv.trees:
        if tree.kind in ("lane", "fleet") and tree.checkout and not tree.tombstone:
            lane_built[tree.checkout] = max(lane_built.get(tree.checkout, 0.0), tree.last_built)
    for tree in inv.trees:
        acts, why = classify(tree, inv, th, me, now, build_tree, lane_built)
        actions.extend(acts)
        if why:
            kept[tree.path] = why
    actions.sort(key=lambda a: (a.rung, a.group, a.last_built, a.tree))
    return actions, kept


def plan(inv: Inventory, free: int, need: int, th: Thresholds, me: Optional[str], now: float,
         build_tree: Optional[str] = None) -> Plan:
    """The reclaim a build of `need` bytes would trigger at `free` bytes free, or a refusal.

    Below the comfort line the ladder's rungs 0-5 run until projected free space reaches comfort +
    hysteresis. Rung 6 is added only if that still leaves less than need + reserve, and stops at
    need + reserve. A refusal means the whole ladder cannot free the build's estimate plus the reserve.

    The `--stale` sweep cannot be sized ahead, so it counts as ZERO here: a plan whose sized actions fall
    short is a refusal even when the sweep is in it. The sweep stays in the action list, and the caller
    runs it in its rung-5 slot, re-reads free space and asks `settle` (a sweep that rescues the build
    turns the refusal into `ok` there, on measured numbers instead of a guess).
    """
    wanted = need + th.reserve
    if free >= max(th.comfort, wanted):
        return Plan("ok", (), free, need, th.reserve, th.comfort, th.comfort + th.hysteresis, free, False,
                    "%s free is above the comfort line and the build's %s plus %s reserve" % (
                        gib(free), gib(need), gib(th.reserve)))
    goal = max(th.comfort + th.hysteresis, wanted)
    candidates, _ = ladder(inv, th, me, now, build_tree)
    chosen: List[Action] = []
    projected = free
    for action in candidates:
        if action.rung > 5:
            continue
        if projected >= goal:
            break
        chosen.append(action)
        projected += action.frees
    if projected < wanted:
        for action in candidates:
            if action.rung != 6:
                continue
            if projected >= wanted:
                break
            chosen.append(action)
            projected += action.frees
    uncertain = any(not a.sized for a in chosen)
    if not chosen and projected >= wanted:
        return Plan("ok", (), free, need, th.reserve, th.comfort, goal, free, False,
                    "%s free is below the comfort line (%s) but nothing is eligible to evict, and the build "
                    "fits (%s with the reserve)" % (gib(free), gib(th.comfort), gib(wanted)))
    if projected >= wanted:
        reason = "%d action(s) free %s, taking %s free to %s" % (
            len(chosen), gib(projected - free), gib(free), gib(projected))
        return Plan("reclaim", tuple(chosen), free, need, th.reserve, th.comfort, goal, projected, uncertain,
                    reason)
    live = sorted(((t.path, t.private_bytes, "; ".join(t.live.reasons())) for t in inv.trees
                   if t.live.reasons() and not never(t, inv)), key=lambda r: -r[1])[:3]
    reason = ("cannot free enough: the build needs ~%s plus a %s reserve (%s), %s is free and the whole "
              "ladder frees at most %s more" % (gib(need), gib(th.reserve), gib(wanted), gib(free),
                                                 gib(projected - free)))
    if uncertain:
        reason += ("; the `--stale` sweep is in the plan but not sized (counted as 0), so a real run may still "
                   "make room: run the plan and let `settle` judge the free space it re-reads")
    return Plan("refuse", tuple(chosen), free, need, th.reserve, th.comfort, goal, projected, uncertain, reason,
                tuple(live))


def settle(need: int, reserve: int, free_after: int) -> Tuple[str, str]:
    """The verdict on free space RE-READ after a plan ran: `ok` once the build's estimate plus the reserve
    fits, else `refuse`. The one rule shared by `make disk`'s promise and `build-gc.sh --evict`'s last line."""
    wanted = need + reserve
    if free_after >= wanted:
        return "ok", "%s free covers the build's %s plus the %s reserve" % (gib(free_after), gib(need), gib(reserve))
    return "refuse", "%s free is short of the build's %s plus the %s reserve (%s)" % (
        gib(free_after), gib(need), gib(reserve), gib(wanted))


# --- formatting helpers ------------------------------------------------------------------------------
def gib(n: float) -> str:
    return "%.1f GiB" % (n / GIB)


def size_text(n: int) -> str:
    if n >= GIB:
        return "%.1fG" % (n / GIB)
    if n >= MIB:
        return "%dM" % round(n / MIB)
    if n > 0:
        return "%dK" % max(1, round(n / 1024))
    return "0"


def age_text(secs: float) -> str:
    secs = max(0.0, secs)
    if secs < 90:
        return "%ds" % secs
    if secs < 5400:
        return "%dm" % round(secs / 60)
    if secs < 2 * 86400:
        return "%dh" % round(secs / 3600)
    return "%dd" % round(secs / 86400)


def tilde(path: str) -> str:
    """A path as the report prints it: ~ for the home directory, /tmp for macOS's /private/tmp."""
    home = os.path.expanduser("~")
    if home and home != "/" and (path == home or path.startswith(home + os.sep)):
        return "~" + path[len(home):]
    if path == "/private/tmp" or path.startswith("/private/tmp/"):
        return path[len("/private"):]
    return path


def tail_text(text: str, width: int) -> str:
    """`text` cut from the LEFT to `width` columns: the end of a path is the part that tells trees apart."""
    return text if len(text) <= width else "..." + text[len(text) - width + 3:]


# --- the inventory: everything below touches the disk ------------------------------------------------
def real(path: str) -> str:
    return os.path.realpath(path)


def int_env(env, name: str, default: int) -> int:
    value = env.get(name, "")
    return int(value) if value.isdigit() else default


def thresholds_from_env(env) -> Thresholds:
    return Thresholds(
        comfort=int_env(env, "PLX_GC_MIN_FREE_GIB", DEFAULT_COMFORT_GIB) * GIB,
        reserve=int_env(env, "PLX_MIN_FREE_MB", DEFAULT_RESERVE_MIB) * MIB,
    )


def estimate_need(tree_path: str, tree_private: int, env) -> int:
    """What building in `tree_path` is expected to add: the estimate minus what is already there."""
    override = env.get("PLX_DISK_NEED_GIB", "")
    try:
        if override:
            return int(float(override) * GIB)
    except ValueError:
        pass
    estimate = NEED_GIB.get(os.path.basename(tree_path), DEFAULT_NEED_GIB)
    return int(max(MIN_NEED_GIB, estimate - tree_private / GIB) * GIB)


def free_bytes(root: str, env) -> Tuple[int, int]:
    """(free, total) of the volume holding `root`. PLX_GC_TEST_FREE_KIB overrides free, as in build-gc."""
    usage = shutil.disk_usage(root)
    override = env.get("PLX_GC_TEST_FREE_KIB", "")
    return (int(override) * 1024 if override.isdigit() else usage.free), usage.total


def load_tool(filename: str):
    """A sibling tool with a hyphen in its name, imported by path so its primitives are reused, not copied."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), filename)
    spec = importlib.util.spec_from_file_location("plx_" + re.sub(r"\W", "_", filename), path)
    module = importlib.util.module_from_spec(spec)
    keep, sys.dont_write_bytecode = sys.dont_write_bytecode, True     # a report leaves no __pycache__ behind
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = keep
    return module


def git_env() -> Dict[str, str]:
    """The environment for a git call: no repository selection inherited from a hook or a test, and
    GIT_OPTIONAL_LOCKS=0 so `git status` does not refresh (write) a lane's index on the way past."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_") or k.startswith("GIT_CONFIG")}
    env["GIT_OPTIONAL_LOCKS"] = "0"
    return env


def git(args: Sequence[str], cwd: str, env: Optional[Dict[str, str]] = None, timeout: int = 60):
    try:
        return subprocess.run(["git", "-C", cwd, *args], capture_output=True, text=True, timeout=timeout,
                              env=env if env is not None else git_env())
    except (OSError, subprocess.SubprocessError):
        return None


@dataclass(frozen=True)
class Worktree:
    path: str
    sha: str
    branch: str
    locked: bool


def worktrees(root: str) -> Tuple[List[Worktree], str]:
    """(every existing worktree, the main checkout), from `git worktree list --porcelain`."""
    out = git(["worktree", "list", "--porcelain"], root)
    common = git(["rev-parse", "--path-format=absolute", "--git-common-dir"], root)
    found: List[Worktree] = []
    if out is not None and out.returncode == 0:
        for block in out.stdout.split("\n\n"):
            path, sha, branch, locked = "", "", "(detached)", False
            for line in block.splitlines():
                if line.startswith("worktree "):
                    path = line[len("worktree "):]
                elif line.startswith("HEAD "):
                    sha = line.split()[1]
                elif line.startswith("branch refs/heads/"):
                    branch = line[len("branch refs/heads/"):]
                elif line.startswith("locked"):
                    locked = True
            if path and os.path.isdir(path):
                found.append(Worktree(real(path), sha, branch, locked))
    main = real(root)
    if common is not None and common.returncode == 0 and common.stdout.strip().endswith("/.git"):
        main = real(common.stdout.strip()[:-len("/.git")])
    return found, main


def base_ref(repo: str) -> str:
    """The ref lanes are judged against: origin/main when present (no fetch), else the local main.
    The local `main` can be days behind it, and then every squash-merged lane reads "unmerged"."""
    for ref in ("refs/remotes/origin/main", "refs/heads/main"):
        probe = git(["rev-parse", "--verify", "--quiet", ref + "^{commit}"], repo)
        if probe is not None and probe.returncode == 0:
            return ref
    return ""


def is_merged(repo: str, sha: str, ref: str) -> bool:
    """Is everything `sha` holds already in `ref`: an ancestor, or a lane whose squash left no difference.

    The squash check is a three-way `git merge-tree --write-tree`, which WRITES the merged tree into
    the object database. A report is read-only, so the writes go to a throwaway object directory (the
    real one is an alternate to read from). One residue remains: when the merged tree already exists
    in the real store (which is exactly the "already landed" answer) git refreshes that object's
    mtime instead of writing it again. No object is added, changed or removed."""
    if not ref or not sha:
        return False
    ancestor = git(["merge-base", "--is-ancestor", sha, ref], repo)
    if ancestor is not None and ancestor.returncode == 0:
        return True
    objects = git(["rev-parse", "--path-format=absolute", "--git-path", "objects"], repo)
    want = git(["rev-parse", ref + "^{tree}"], repo)
    if objects is None or want is None or objects.returncode or want.returncode:
        return False
    with tempfile.TemporaryDirectory(prefix="plx-disk-budget-") as scratch:
        env = git_env()
        env["GIT_OBJECT_DIRECTORY"] = scratch
        env["GIT_ALTERNATE_OBJECT_DIRECTORIES"] = objects.stdout.strip()
        merged = git(["merge-tree", "--write-tree", "--no-messages", ref, sha], repo, env)
    if merged is None or merged.returncode != 0 or not merged.stdout.strip():
        return False
    return merged.stdout.split()[0] == want.stdout.strip()


def is_clean(path: str) -> bool:
    status = git(["status", "--porcelain"], path)
    return status is not None and status.returncode == 0 and not status.stdout.strip()


def tracked_under(tree: str) -> bool:
    out = git(["ls-files", "-z", "--", "."], tree)
    return out is not None and out.returncode == 0 and bool(out.stdout)


def subdirs(path: str) -> List[str]:
    try:
        with os.scandir(path) as it:
            return [e.path for e in it if e.is_dir(follow_symlinks=False)]
    except OSError:
        return []


def profile_dirs(tree: str) -> List[str]:
    """`<tree>/<profile>` and `<tree>/<triple>/<profile>`: the dirs cargo keeps deps/.fingerprint/... in."""
    out = []
    for child in subdirs(tree):
        if any(os.path.isdir(os.path.join(child, s)) for s in PROFILE_SUBDIRS):
            out.append(child)
            continue
        for grand in subdirs(child):
            if any(os.path.isdir(os.path.join(grand, s)) for s in PROFILE_SUBDIRS):
                out.append(grand)
    return out


def has_plx_marker(tree: str) -> bool:
    for profile in profile_dirs(tree):
        for sub in PROFILE_SUBDIRS:
            try:
                with os.scandir(os.path.join(profile, sub)) as it:
                    if any(e.name.startswith(PLX_MARKERS) for e in it):
                        return True
            except OSError:
                continue
    return False


def newest_mtime(tree: str, ctime: bool = False) -> float:
    """When cargo last wrote in `tree`: directory-entry stats of deps/.fingerprint/build/incremental in
    each profile dir, no full walk. A directory's own mtime does not move while a compiler rewrites a
    file inside it, hence the entries.

    With `ctime`, the later of each entry's mtime and ctime. An mtime can be set to anything (`os.utime`,
    `cp -p`, a restore from an archive, a test fixture); a ctime is the kernel's and cannot be set, so a
    tree that was merely touched back into the past still reads as recently changed."""
    def stamp(st: os.stat_result) -> float:
        return max(st.st_mtime, st.st_ctime) if ctime else st.st_mtime

    try:
        newest = stamp(os.lstat(tree))
    except OSError:
        return 0.0
    for profile in profile_dirs(tree):
        for sub in PROFILE_SUBDIRS:
            d = os.path.join(profile, sub)
            try:
                newest = max(newest, stamp(os.lstat(d)))
                with os.scandir(d) as it:
                    for e in it:
                        try:
                            newest = max(newest, stamp(e.stat(follow_symlinks=False)))
                        except OSError:
                            pass
            except OSError:
                continue
    return newest


_PRIVATE = {"fn": None, "attrs": None, "ok": sys.platform == "darwin"}


def private_size(path: str, st: os.stat_result) -> int:
    """Bytes of this file no other file shares (APFS ATTR_CMNEXT_PRIVATESIZE); allocated blocks where
    that cannot be asked. `du` counts a clone in full in every tree holding it, this does not."""
    fallback = st.st_blocks * 512
    if not _PRIVATE["ok"]:
        return fallback
    try:
        if _PRIVATE["fn"] is None:
            class AttrList(ctypes.Structure):
                _fields_ = [("bitmapcount", ctypes.c_ushort), ("reserved", ctypes.c_uint16),
                            ("commonattr", ctypes.c_uint32), ("volattr", ctypes.c_uint32),
                            ("dirattr", ctypes.c_uint32), ("fileattr", ctypes.c_uint32),
                            ("forkattr", ctypes.c_uint32)]
            fn = ctypes.CDLL(None, use_errno=True).getattrlist
            fn.argtypes = [ctypes.c_char_p, ctypes.POINTER(AttrList), ctypes.c_void_p, ctypes.c_size_t,
                           ctypes.c_uint32]
            fn.restype = ctypes.c_int
            # bitmap count 5, forkattr = ATTR_CMNEXT_PRIVATESIZE (0x8); options NOFOLLOW | CMN_EXTENDED
            _PRIVATE["attrs"] = AttrList(5, 0, 0, 0, 0, 0, 0x8)
            _PRIVATE["fn"] = fn
        buf = ctypes.create_string_buffer(32)
        if _PRIVATE["fn"](os.fsencode(path), ctypes.byref(_PRIVATE["attrs"]), buf, 32, 0x1 | 0x20) != 0:
            return fallback
        return max(0, struct.unpack_from("<Iq", buf.raw)[1])
    except (AttributeError, OSError, ValueError, struct.error):
        _PRIVATE["ok"] = False
        return fallback


def measure(tree: str, incr_roots: Sequence[str]) -> Tuple[int, int, int]:
    """(private bytes of the whole tree, allocated bytes, private bytes of the incremental dirs).

    One lstat per name, one private-size query per inode. An inode is freed by a deletion only when every
    one of its names (st_nlink) lies inside what is deleted, so a file hard-linked from `incremental/`
    into `deps/` is freed by the tree and not by `incremental/` alone."""
    inodes: Dict[Tuple[int, int], List[int]] = {}   # -> [private, nlink, allocated, names in tree, names in incr]
    incr = tuple(r.rstrip(os.sep) + os.sep for r in incr_roots)
    for dirpath, _dirs, files in os.walk(tree, followlinks=False):
        in_incr = bool(incr) and (dirpath + os.sep).startswith(incr)
        for name in files:
            full = os.path.join(dirpath, name)
            try:
                st = os.lstat(full)
            except OSError:
                continue
            if not stat.S_ISREG(st.st_mode):
                continue
            key = (st.st_dev, st.st_ino)
            rec = inodes.get(key)
            if rec is None:
                rec = inodes[key] = [private_size(full, st), st.st_nlink, st.st_blocks * 512, 0, 0]
            rec[3] += 1
            if in_incr:
                rec[4] += 1
    private = sum(r[0] for r in inodes.values() if r[3] >= r[1])
    alloc = sum(r[2] for r in inodes.values())
    incr_private = sum(r[0] for r in inodes.values() if r[4] and r[4] >= r[1])
    return private, alloc, incr_private


def discover_tmp_trees(roots: Sequence[str], skip: Sequence[str]) -> List[str]:
    """Directories carrying a CACHEDIR.TAG at most TMP_DEPTH below a root, as
    `find -maxdepth 4 -name CACHEDIR.TAG` finds them. Registered checkouts are not entered: their trees
    are enumerated through git. A tagged directory that holds one (or the fleet dir) is not listed."""
    found: List[str] = []
    seen = set()
    skipped = [real(x) for x in skip]
    for root in roots:
        root = real(root)
        if root in seen or not os.path.isdir(root):
            continue
        seen.add(root)
        # a root that IS, or lies inside, a skipped tree (the fleet directory, a registered checkout) is
        # somebody else's inventory
        if any(root == x or root.startswith(x + os.sep) for x in skipped):
            continue
        stack = [(root, 0)]
        while stack:
            directory, depth = stack.pop()
            try:
                entries = list(os.scandir(directory))
            except OSError:
                continue
            if any(e.name == "CACHEDIR.TAG" for e in entries):
                # a tagged tree that HOLDS a registered checkout (or the fleet dir) is not a build tree to
                # delete: the checkout's source would go with it, and the tracked-file probe cannot see a
                # repository nested below the tree it is asked about
                if not any(x.startswith(directory + os.sep) for x in skipped):
                    found.append(directory)
                continue
            if depth >= TMP_DEPTH:
                continue
            for e in entries:
                if e.name in (".git", "node_modules"):
                    continue
                try:
                    if e.is_dir(follow_symlinks=False) and real(e.path) not in skipped:
                        stack.append((e.path, depth + 1))
                except OSError:
                    continue
    return sorted(found)


def tmp_roots(env) -> List[str]:
    """Where PlxNative cargo trees outside any checkout are looked for: PLX_GC_TMP_ROOTS (an os.pathsep
    list; an empty value means the defaults), else /tmp and $TMPDIR."""
    explicit = [p for p in (env.get("PLX_GC_TMP_ROOTS") or "").split(os.pathsep) if p]
    return explicit or ["/tmp"] + ([env["TMPDIR"]] if env.get("TMPDIR") else [])


def fleet_dir(env) -> str:
    """$PLX_FLEET_DIR, which may be set to the empty string to mean "no fleet"; unset means ~/plx-fleet."""
    value = env.get("PLX_FLEET_DIR")
    return os.path.expanduser("~/plx-fleet") if value is None else value


def build_cache(env) -> str:
    return real(env.get("PLX_BUILD_CACHE") or os.path.expanduser("~/.cache/plxnative"))


def tmp_candidates(env, checkouts: Sequence[str]) -> List[str]:
    """Every cargo-tagged directory under the tmp roots that is not a checkout, nor inside the fleet dir."""
    fleet = fleet_dir(env)
    skip = list(checkouts) + ([real(fleet)] if fleet and os.path.isdir(fleet) else [])
    return discover_tmp_trees(tmp_roots(env), skip)


def flock_held(path: str) -> bool:
    """Is an exclusive flock outstanding on `path`? Opened read-only (a probe must create nothing)."""
    try:
        fd = os.open(path, os.O_RDONLY)
    except OSError:
        return False
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
            fcntl.flock(fd, fcntl.LOCK_UN)
            return False
        except OSError:
            return True
    finally:
        os.close(fd)


@dataclass
class LiveSnapshot:
    builders: List[Tuple[str, str]]       # (command, cwd) of every builder process of this user
    files: List[Tuple[str, str]]          # (command, path) of every open regular file
    unknown: bool = False


def parse_lsof(text: str, ignore_pids: Sequence[int] = ()) -> LiveSnapshot:
    """`lsof -F pcfn` output: p<pid> c<command> f<fd> n<name>, the f/n pair repeating per file."""
    builders, files = [], []
    pid, command, fd = 0, "", ""
    for line in text.splitlines():
        if not line:
            continue
        tag, value = line[0], line[1:]
        if tag == "p":
            pid = int(value) if value.isdigit() else 0
        elif tag == "c":
            command = value
        elif tag == "f":
            fd = value
        elif tag == "n" and pid not in ignore_pids and value.startswith("/"):
            if fd == "cwd":
                if command in BUILDER_COMMANDS:
                    builders.append((command, real(value)))
            elif fd != "rtd":
                files.append((command, value))
    return LiveSnapshot(builders, files)


def ancestor_pids() -> List[int]:
    """This process and its parents: a `make disk` running in a checkout is not a build in it."""
    pids, pid = [], os.getpid()
    for _ in range(32):
        if pid <= 1:
            break
        pids.append(pid)
        out = subprocess.run(["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True)
        pid = int(out.stdout.strip()) if out.stdout.strip().isdigit() else 0
    return pids


def interpret_lsof(returncode: int, stdout: str, ignore_pids: Sequence[int] = ()) -> LiveSnapshot:
    """What an `lsof -u <me> -F pcfn` run says is live, or `unknown` when it cannot be trusted.

    macOS `lsof` exits 1 when some file it listed was unreadable but still prints the rest, so 0 and 1 both
    count. A run that printed no process record at all is NOT "nothing is running": this very process has
    a working directory, so a working `lsof` always lists at least one. Empty output means it did not work
    (a sandbox, a missing permission, a stub), and ignorance must read as live, not as idle."""
    if returncode not in (0, 1) or not re.search(r"^p\d+$", stdout, re.M):
        return LiveSnapshot([], [], unknown=True)
    return parse_lsof(stdout, ignore_pids)


class Probes:
    """Everything the inventory asks of the running machine. A test passes its own."""

    def __init__(self, env=None):
        self.env = os.environ if env is None else env
        self._tools: Dict[str, object] = {}

    def tool(self, filename: str):
        if filename not in self._tools:
            self._tools[filename] = load_tool(filename)
        return self._tools[filename]

    def snapshot(self) -> LiveSnapshot:
        try:
            out = subprocess.run(["lsof", "-n", "-P", "-w", "+c", "0", "-u", str(os.getuid()), "-F", "pcfn"],
                                 capture_output=True, text=True, timeout=60)
        except (OSError, subprocess.SubprocessError):
            return LiveSnapshot([], [], unknown=True)
        return interpret_lsof(out.returncode, out.stdout, ancestor_pids())

    def cargo_locked(self, tree: str) -> bool:
        """A cargo build lock under `tree` is held: cargo-seed's own probe, shared locks taken and dropped."""
        try:
            with self.tool("cargo-seed.py").CargoLocks(tree) as locks:
                return not locks.idle
        except Exception:  # noqa: BLE001 - a probe that cannot run must not read as "idle"
            return True

    def check_locked(self, checkout: str) -> bool:
        """Is `make check` running in this checkout (check-lock.py's per-worktree flock)?"""
        if self.env.get("PLX_CHECK_LOCK") == "off":
            return False
        try:
            lock = self.tool("check-lock.py")
            return flock_held(lock.worktree_lock_path(lock.default_lock_path(), checkout))
        except Exception:  # noqa: BLE001
            return False

    def seed_locked(self, seed: str) -> bool:
        return flock_held(seed.rstrip(os.sep) + ".lock")


def collect(root: str, env, probes: Probes, now: float, size_foreign: bool = False) -> Inventory:
    """Walk the disk and build the inventory. Reads only: no cache, no lock file, no object is written."""
    root = real(root)
    wts, main = worktrees(root)
    if not any(w.path == root for w in wts):
        wts.append(Worktree(root, "", "(detached)", False))
    ref = base_ref(root)
    names = {w.path: ("main" if w.path == main else os.path.basename(w.path)) for w in wts}
    cache = build_cache(env)
    fleet = fleet_dir(env)
    volume_dev = os.stat(root).st_dev

    def owning(path: str) -> Optional[str]:
        best = None
        for w in wts:
            if under(path, w.path) and (best is None or len(w.path) > len(best)):
                best = w.path
        return best

    # (path, kind, checkout, owner, orphan) for every candidate, deduplicated by realpath.
    seen: Dict[str, Tuple[str, Optional[str], str, bool]] = {}
    lane_facts: Dict[str, Tuple[bool, bool, bool]] = {}
    for w in wts:
        for d in sorted(subdirs(os.path.join(w.path, "rust-modules"))):
            if os.path.basename(d).startswith("target"):
                seen.setdefault(real(d), ("main" if w.path == main else "lane", w.path, names[w.path], False))
    lane_names = {os.path.basename(w.path): w.path for w in wts}
    if fleet and os.path.isdir(fleet):
        for lane in sorted(subdirs(fleet)):
            for d in sorted(subdirs(lane)):
                if os.path.basename(d).startswith("target"):
                    owner = lane_names.get(os.path.basename(lane))
                    seen.setdefault(real(d), ("fleet", owner, os.path.basename(lane), owner is None))
    for d in tmp_candidates(env, [w.path for w in wts]):
        seen.setdefault(real(d), ("tmp", owning(real(d)), "", False))

    for kind_dir in ("ffmpeg", "cargo-seed"):
        for d in sorted(subdirs(os.path.join(cache, kind_dir))):
            seen.setdefault(real(d), ("cache", None, "", False))

    snap = probes.snapshot()
    trees: List[Tree] = []
    for path, (kind, checkout, owner, orphan) in sorted(seen.items()):
        base = os.path.basename(path)
        if kind == "cache":
            parent = os.path.basename(os.path.dirname(path))
            tombstone = ".tombstone." in base
            leftover = tombstone or (parent == "cargo-seed" and any(m in base for m in LEFTOVER_MARKS))
            is_seed = parent == "cargo-seed" and not leftover and "." not in base
            if not (leftover or is_seed):
                continue                      # the FFmpeg cache's live entries are never ours to plan
            try:
                st = os.lstat(path)
            except OSError:
                continue
            if leftover and now - st.st_mtime < LEFTOVER_SECS and not tombstone:
                continue                      # an in-flight harvest or restore
            private, alloc, _ = measure(path, ())
            stamp = os.path.join(path, ".last-used")
            built = os.lstat(stamp).st_mtime if os.path.exists(stamp) else st.st_mtime
            locked = probes.seed_locked(path) if is_seed else False
            trees.append(Tree(path=path, kind="leftover" if leftover else "seed", dev=st.st_dev,
                              private_bytes=private, alloc_bytes=alloc, last_built=built,
                              tombstone=tombstone, live=live_for(path, None, None, snap, locked, False)))
            continue
        plx = kind != "tmp" or has_plx_marker(path)
        if not plx and not size_foreign:
            trees.append(Tree(path=path, kind="tmp", plx=False, dev=os.stat(path).st_dev,
                              last_built=newest_mtime(path)))
            continue
        # A /tmp tree's age is the later of mtime and ctime (docs/disk-budget.md): a test fixture whose
        # files were `utime`d into the past is not "garbage nobody has written to for a day".
        built = newest_mtime(path, ctime=(kind == "tmp"))
        incr = tuple(sorted(os.path.join(p, "incremental") for p in profile_dirs(path)
                            if os.path.isdir(os.path.join(p, "incremental"))))
        private, alloc, incr_private = measure(path, incr)
        merged = clean = locked = False
        if kind in ("lane", "fleet") and checkout and checkout != main and checkout != root:
            if checkout not in lane_facts:
                wt = next(w for w in wts if w.path == checkout)
                is_m = is_merged(checkout, wt.sha, ref)
                lane_facts[checkout] = (is_m, is_m and is_clean(checkout), wt.locked)
            merged, clean, locked = lane_facts[checkout]
        elif kind in ("lane", "fleet") and checkout:
            locked = next((w.locked for w in wts if w.path == checkout), False)
        trees.append(Tree(
            path=path, kind=kind, checkout=checkout, owner=owner, orphan=orphan, plx=plx,
            tombstone=".tombstone." in base, tracked=tracked_under(path), dev=os.stat(path).st_dev,
            private_bytes=private if plx else 0, alloc_bytes=alloc, incr_paths=incr if plx else (),
            incr_bytes=incr_private if plx else 0, last_built=built,
            live=live_for(path, checkout, kind == "fleet" and owner or None, snap,
                          probes.cargo_locked(path), probes.check_locked(checkout) if checkout else False,
                          [w.path for w in wts]),
            merged=merged, clean=clean, locked=locked))
    protected = []
    for w in wts:
        protected += [os.path.join(w.path, "pkg"), os.path.join(w.path, "vendor")]
    protected.append(os.path.join(cache, "ffmpeg"))
    return Inventory(tuple(trees), volume_dev, tuple(protected), cache, ref,
                     tuple((w.path, names[w.path]) for w in wts))


def live_for(path: str, checkout: Optional[str], lane: Optional[str], snap: LiveSnapshot, cargo_locked: bool,
             check_locked: bool, checkouts: Sequence[str] = ()) -> Liveness:
    builder = ""
    for command, cwd in snap.builders:
        owner = None
        for c in checkouts:
            if under(cwd, c) and (owner is None or len(c) > len(owner)):
                owner = c
        if checkout and owner == checkout:
            builder = command
        elif under(cwd, path):
            builder = command
        elif lane and ("/%s/" % lane) in (cwd + "/"):
            builder = command     # build-gc's second rule: a lane torn down while its build still runs
    opened = tuple(sorted({command for command, name in snap.files if under(name, path)}))
    return Liveness(builder, opened, cargo_locked, check_locked, snap.unknown)


# --- the report --------------------------------------------------------------------------------------
def log_tail(env, count: int = 5) -> List[str]:
    path = env.get("PLX_GC_LOG")
    if not path:
        path = os.path.expanduser("~/Library/Logs/plxnative-build-gc.log") if sys.platform == "darwin" else \
            os.path.join(env.get("XDG_STATE_HOME") or os.path.expanduser("~/.cache"), "plxnative",
                         "build-gc.log")
    try:
        with open(path, errors="replace") as fh:
            return [line.rstrip("\n") for line in fh.readlines()[-count:]]
    except OSError:
        return []


def render(inv: Inventory, th: Thresholds, free: int, total: int, me: str, build_tree: str, need: int,
           now: float, env) -> str:
    pl = plan(inv, free, need, th, me, now, build_tree)
    actions, kept = ladder(inv, th, me, now, build_tree)
    lines = ["== disk budget (tools/disk-budget.py report: read-only, deletes nothing) =="]
    lines.append("volume        %s free of %s" % (gib(free), gib(total)))
    lines.append("comfort line  %s free (PLX_GC_MIN_FREE_GIB); a reclaim runs up to %s (+%s hysteresis)" % (
        gib(th.comfort), gib(th.comfort + th.hysteresis), gib(th.hysteresis)))
    lines.append("reserve       %s (PLX_MIN_FREE_MB)" % gib(th.reserve))
    lines.append("next build    ~%s into %s (PLX_DISK_NEED_GIB overrides); needs %s with the reserve" % (
        gib(need), tilde(build_tree), gib(need + th.reserve)))
    lines.append("lanes judged  against %s" % (inv.base_ref.replace("refs/remotes/", "").replace("refs/heads/", "")
                                               or "(no main ref found: no lane reads as merged)"))
    if pl.verdict == "ok":
        lines.append("verdict       ok: %s" % pl.reason)
    elif pl.verdict == "reclaim":
        lines.append("verdict       would reclaim: %s" % pl.reason)
    else:
        lines.append("verdict       WOULD REFUSE: %s" % pl.reason)
        for path, size, why in pl.biggest_live:
            lines.append("              live: %s  %s  (%s)" % (size_text(size), tilde(path), why))
    lines.append("")

    planned = {(a.tree, a.scope) for a in pl.actions}
    rungs_of: Dict[str, List[int]] = {}
    for a in actions:
        rungs_of.setdefault(a.tree, [])
        if a.rung not in rungs_of[a.tree]:
            rungs_of[a.tree].append(a.rung)
    lines.append("trees  (PRIVATE is what deleting it frees; DU counts APFS clones in full, in every tree)")
    lines.append("  %-9s %-26s %-34s %8s %8s %6s  %s" % ("KIND", "OWNER", "TREE", "PRIVATE", "DU", "BUILT", "STATE"))
    order = {"main": 0, "lane": 1, "fleet": 2, "tmp": 3, "seed": 4, "leftover": 5}
    for t in sorted(inv.trees, key=lambda t: (order.get(t.kind, 9), t.owner, t.path)):
        if not t.plx:
            state = "not ours: no PlxNative marker, never touched"
        elif t.path in kept and not rungs_of.get(t.path):
            state = kept[t.path]
        else:
            state = "rung " + ",".join(str(r) for r in sorted(rungs_of.get(t.path, [])))
            if t.path in kept:
                state += "; " + kept[t.path]
        marks = [m for m, on in (("current", me and t.checkout == me), ("locked", t.locked),
                                 ("merged", t.merged and t.clean), ("orphan", t.orphan)) if on]
        tag = " [" + ",".join(marks) + "]" if marks else ""
        owner = (t.owner or "-")[:max(6, 26 - len(tag))] + tag
        shown = os.path.basename(t.path) if t.kind in ("lane", "main", "fleet") else tilde(t.path)
        lines.append("  %-9s %-26s %-34s %8s %8s %6s  %s" % (
            t.kind, owner, tail_text(shown, 34), size_text(t.private_bytes) if t.plx else "-",
            size_text(t.alloc_bytes) if t.alloc_bytes else "-", age_text(now - t.last_built), state))
    lines.append("")

    lines.append("reclaim ladder  (the order a reclaim WOULD take, LRU within a rung; * = chosen at the free "
                 "space above)")
    lines.append("  %-2s %-4s %-11s %8s %8s  %-22s %s" % ("", "RUNG", "ACTION", "FREES", "TOTAL", "OWNER", "TREE"))
    cumulative = 0
    for a in actions:
        cumulative += a.frees
        mark = "*" if (a.tree, a.scope) in planned else " "
        frees = size_text(a.frees) if a.sized else "?"
        lines.append("  %-2s %-4d %-11s %8s %8s  %-22s %s  (%s)" % (
            mark, a.rung, a.scope, frees, size_text(cumulative), a.owner[:22], tilde(a.tree), a.why))
    if not actions:
        lines.append("  (nothing is eligible: every tree is current, live, kept or has nothing private to free)")
    for rung, text in sorted(SKIPPED_RUNGS.items()):
        lines.append("  rung %d skipped: %s" % (rung, text))
    lines.append("  a `?` is not estimated (the `--stale` sweep sizes itself); rung 6 runs only when the build "
                 "would otherwise be refused")
    lines.append("  never planned: the current checkout (rung 6 aside), live trees, tracked files, pkg/, vendor/, "
                 "the FFmpeg cache's live entries, non-PlxNative trees")
    tail = log_tail(env)
    if tail:
        lines.append("")
        lines.append("last build-gc log lines:")
        lines.extend("  " + tilde(line) for line in tail)
    return "\n".join(lines)


def default_root() -> str:
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


# --- the plan, as the one deleting tool reads it ------------------------------------------------------
PLAN_HEADER = "# plx-disk-plan v1"
PLAN_FIELDS = 8


def emit(pl: Plan) -> str:
    """The plan as `build-gc.sh --evict` reads it: `#` header lines, then one tab-separated line per path

        evict  RUNG  SCOPE  EST_KIB  OWNER|-  IDLE_MIN  MODE  PATH

    in the order to run them, and a single `stale` line (same fields) at the `--stale` sweep's rung-5 slot.
    EST_KIB is the action's private bytes and rides on its first path. IDLE_MIN is how long the owner must
    have been idle when the executor looks again (0: no age gate). MODE is `current` for rung 6's
    current-checkout group, whose liveness rule differs (`make` itself runs there), else `normal`.
    The plan is a proposal: `--evict` treats every field as untrusted and re-checks each path."""
    lines = [PLAN_HEADER, "# verdict %s" % pl.verdict, "# free_kib %d" % (pl.free // 1024),
             "# goal_kib %d" % (pl.goal // 1024), "# wanted_kib %d" % ((pl.need + pl.reserve) // 1024),
             "# need_kib %d" % (pl.need // 1024), "# reserve_kib %d" % (pl.reserve // 1024)]
    stale_done = False
    for a in pl.actions:
        mode = "current" if a.group == 1 else "normal"
        owner = a.owner or "-"
        if a.scope == "stale":
            if not stale_done:
                stale_done = True
                lines.append("\t".join(("stale", str(a.rung), "stale", "0", owner, "0", mode, a.tree)))
            continue
        for index, path in enumerate(a.paths):
            if "\t" in path or "\n" in path or "\r" in path:
                lines.append("# skipped: a path with a control character in its name (%r)" % path)
                continue
            lines.append("\t".join(("evict", str(a.rung), a.scope, str((a.frees // 1024) if index == 0 else 0),
                                    owner, str(a.idle_secs // 60), mode, path)))
    return "\n".join(lines)


def read_plan_header(text: str) -> Dict[str, str]:
    out: Dict[str, str] = {}
    for line in text.splitlines():
        if line.startswith("# ") and " " in line[2:]:
            key, value = line[2:].split(" ", 1)
            out[key] = value.strip()
    return out


def render_plan(pl: Plan) -> str:
    lines = ["verdict  %s: %s" % (pl.verdict, pl.reason),
             "free     %s; goal %s; the build needs %s with the %s reserve" % (
                 gib(pl.free), gib(pl.goal), gib(pl.need + pl.reserve), gib(pl.reserve))]
    for a in pl.actions:
        lines.append("  rung %d  %-11s %8s  %-22s %s  (%s)" % (
            a.rung, a.scope, size_text(a.frees) if a.sized else "?", a.owner[:22], tilde(a.tree), a.why))
    if not pl.actions:
        lines.append("  (nothing to evict)")
    return "\n".join(lines)


# --- test seams, and the guard that keeps them away from the owner's real trees -----------------------
def clock(env) -> float:
    """The time the planner and the probes read. PLX_GC_TEST_NOW (epoch seconds) moves it, for a test that
    must age a tree it just made, and is honoured ONLY when PLX_GC_TMP_ROOTS names the directories to
    look in: a clock set into the future while the defaults (/tmp, $TMPDIR) are searched is how a test
    would read the owner's real trees as ancient garbage."""
    value = env.get("PLX_GC_TEST_NOW", "")
    if value and env.get("PLX_GC_TMP_ROOTS"):
        try:
            return float(value)
        except ValueError:
            pass
    return time.time()


def default_roots(env) -> List[str]:
    """The names among PLX_GC_TMP_ROOTS, PLX_FLEET_DIR and PLX_BUILD_CACHE that are left to their defaults."""
    missing = []
    if not env.get("PLX_GC_TMP_ROOTS"):
        missing.append("PLX_GC_TMP_ROOTS")
    if env.get("PLX_FLEET_DIR") is None:
        missing.append("PLX_FLEET_DIR")
    if not env.get("PLX_BUILD_CACHE"):
        missing.append("PLX_BUILD_CACHE")
    return missing


def refuse_pretend_emergency(env) -> str:
    """Why a plan must not be made right now, or "". A test override (PLX_GC_TEST_FREE_KIB pretends the disk
    is full, PLX_GC_TEST_NOW pretends it is later) over the DEFAULT roots would plan the machine's real
    trees as if they were the emergency; `report` is read-only and unaffected, a plan is not."""
    overrides = [name for name in ("PLX_GC_TEST_FREE_KIB", "PLX_GC_TEST_NOW") if env.get(name)]
    missing = default_roots(env)
    if overrides and missing:
        return ("refusing to plan under %s while %s is left to its default: a pretend emergency over the real "
                "trees is how a test would delete them. Point every root at a disposable directory." % (
                    "+".join(overrides), "+".join(missing)))
    return ""


# --- subcommands --------------------------------------------------------------------------------------
def cmd_report(args, env) -> int:
    root = real(args.root)
    th = thresholds_from_env(env)
    now = clock(env)
    inv = collect(root, env, Probes(env), now, size_foreign=True)
    build_tree = real(args.tree) if args.tree else os.path.join(root, "rust-modules", "target")
    current = next((t.private_bytes for t in inv.trees if t.path == build_tree), 0)
    need = int(args.need_gib * GIB) if args.need_gib is not None else estimate_need(build_tree, current, env)
    free, total = free_bytes(root, env)
    print(render(inv, th, free, total, root, build_tree, need, now, env))
    return 0


def cmd_reclaim(args, env) -> int:
    """The plan for a reclaim up to `--to` GiB free (comfort line = `--to`, goal = `--to` + hysteresis)."""
    refusal = refuse_pretend_emergency(env)
    if refusal:
        print("disk-budget: " + refusal, file=sys.stderr)
        return 2
    root = real(args.root)
    now = clock(env)
    th = replace(thresholds_from_env(env), comfort=int(args.to * GIB))
    if args.idle_min is not None:
        th = replace(th, idle_secs=max(0, args.idle_min) * 60)
    inv = collect(root, env, Probes(env), now)
    free, _ = free_bytes(root, env)
    if args.no_build:
        me, build_tree, need = None, None, int((args.need_gib or 0) * GIB)
    else:
        me = root
        build_tree = real(args.tree) if args.tree else os.path.join(root, "rust-modules", "target")
        current = next((t.private_bytes for t in inv.trees if t.path == build_tree), 0)
        need = int(args.need_gib * GIB) if args.need_gib is not None else estimate_need(build_tree, current, env)
    pl = plan(inv, free, need, th, me, now, build_tree)
    print(emit(pl) if args.emit else render_plan(pl))
    return 0


def cmd_tmp_trees(args, env) -> int:
    """The PlxNative cargo trees outside any checkout and the fleet dir, on the build's volume: the one
    discovery `build-gc.sh --evict` uses to know what a /tmp path may be. One realpath per line."""
    root = real(args.root)
    wts, _ = worktrees(root)
    checkouts = [w.path for w in wts] + [root]
    dev = os.stat(root).st_dev
    for path in sorted({real(p) for p in tmp_candidates(env, checkouts)}):
        try:
            if os.stat(path).st_dev == dev and has_plx_marker(path):
                print(path)
        except OSError:
            continue
    return 0


def probe_tree(tree: str, checkout: Optional[str], lane: Optional[str], klass: str, current: bool,
               min_age: int, probes: Probes, checkouts: Sequence[str], now: float) -> List[str]:
    """Why `tree` must not be deleted right now (empty: nothing says so). The planner's own liveness
    signals, asked again at the moment of deletion: open handles under the tree, a held cargo lock, a held
    `make check` lock for its checkout, and (current=False) a builder whose cwd is in the checkout; for a
    /tmp tree also its age (the later of mtime and ctime) against `min_age`, which never drops below the
    idle window. A tree git tracks a file in is refused for every class."""
    reasons: List[str] = []
    if tracked_under(tree):
        reasons.append("git tracks a file in it")
    snap = probes.snapshot()
    reasons.extend(live_for(tree, checkout, lane, snap, probes.cargo_locked(tree),
                            probes.check_locked(checkout) if checkout else False,
                            checkouts).reasons(current=current))
    if klass == "tmp":
        floor = max(min_age, IDLE_SECS)
        age = now - newest_mtime(tree, ctime=True)
        if age < floor:
            reasons.append("changed %s ago (a /tmp tree must be idle %s)" % (age_text(age), age_text(floor)))
    elif klass == "leftover" and ".tombstone." not in os.path.basename(tree):
        # A `.tmp.` / `.restore.` / `.old.` entry may be an in-flight harvest or restore; only one older than
        # cargo-seed's own limit is a leftover (an interrupted delete's tombstone is never in flight).
        try:
            age = now - os.lstat(tree).st_mtime
        except OSError as err:
            return ["cannot be read: %s" % err]
        if age < LEFTOVER_SECS:
            reasons.append("changed %s ago (a seed-run leftover must be %s old)" % (age_text(age), age_text(LEFTOVER_SECS)))
    return reasons


def cmd_probe(args, env) -> int:
    root = real(args.root)
    wts, _ = worktrees(root)
    checkouts = [w.path for w in wts] + ([root] if root not in [w.path for w in wts] else [])
    reasons = probe_tree(real(args.tree), real(args.checkout) if args.checkout else None, args.lane or None,
                         args.klass, args.current, args.min_age_secs, Probes(env), checkouts, clock(env))
    if reasons:
        print("; ".join(reasons))
        return 1
    print("idle")
    return 0


def cmd_settle(args, env) -> int:
    text = open(args.plan).read() if args.plan != "-" else sys.stdin.read()
    header = read_plan_header(text)
    try:
        need, reserve = int(header["need_kib"]) * 1024, int(header["reserve_kib"]) * 1024
    except (KeyError, ValueError):
        print("n/a: the plan carries no budget")
        return 0
    verdict, why = settle(need, reserve, args.free_kib * 1024)
    print("%s: %s" % (verdict if verdict == "ok" else "REFUSE", why))
    return 0 if verdict == "ok" else 3


def main(argv: Sequence[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="action", required=True)

    def with_root(p):
        p.add_argument("--root", default=default_root(), help="the current checkout (default: this script's)")
        return p

    rp = with_root(sub.add_parser("report", help="print the budget, the trees, the ladder and the verdict "
                                                 "(read-only)"))
    rp.add_argument("--tree", help="the tree the next build writes (default: <root>/rust-modules/target)")
    rp.add_argument("--need-gib", type=float, help="the next build's estimate (default: by tree name)")

    cp = with_root(sub.add_parser("reclaim", help="the plan for a reclaim up to --to GiB free (read-only)"))
    cp.add_argument("--to", type=float, required=True,
                    help="the comfort line in GiB: the reclaim runs up to this plus the hysteresis")
    cp.add_argument("--emit", action="store_true", help="print the tab-separated plan `build-gc.sh --evict` reads")
    cp.add_argument("--no-build", action="store_true",
                    help="no build is about to start (a hook, a timer): no current checkout, need 0")
    cp.add_argument("--idle-min", type=int, help="how long a tree must have been idle (default 15)")
    cp.add_argument("--tree", help="the tree the next build writes (default: <root>/rust-modules/target)")
    cp.add_argument("--need-gib", type=float, help="the next build's estimate (default: by tree name)")

    tp = with_root(sub.add_parser("tmp-trees", help="print the PlxNative cargo trees outside any checkout"))

    pp = with_root(sub.add_parser("probe", help="is this tree safe to delete right now? exit 0: yes"))
    pp.add_argument("--tree", required=True, help="the cargo target directory the path belongs to")
    pp.add_argument("--class", dest="klass", required=True, choices=("lane", "main", "fleet", "tmp", "leftover"))
    pp.add_argument("--checkout", help="the checkout that owns the tree")
    pp.add_argument("--lane", help="the fleet lane the tree belongs to")
    pp.add_argument("--current", action="store_true",
                    help="the tree is in the checkout that is building: ignore its builder and `make check` lock")
    pp.add_argument("--min-age-secs", type=int, default=0, help="a /tmp tree must be unchanged this long")

    sp = sub.add_parser("settle", help="judge free space re-read after a plan ran")
    sp.add_argument("--plan", required=True, help="the plan file (or - for stdin)")
    sp.add_argument("--free-kib", type=int, required=True)

    args = ap.parse_args(argv)
    env = dict(os.environ)
    return {"report": cmd_report, "reclaim": cmd_reclaim, "tmp-trees": cmd_tmp_trees, "probe": cmd_probe,
            "settle": cmd_settle}[args.action](args, env)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
