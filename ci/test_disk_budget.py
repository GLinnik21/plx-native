#!/usr/bin/env python3
"""Tests for tools/disk-budget.py: the planner's order and safety rules, and the inventory under it.

The planner is a pure function, so most of this file feeds `plan` / `ladder` / `never` synthetic
inventories and reads the answer. The properties pinned here are the ones whose failure is a deleted
tree that something needed once the eviction PR acts on these verdicts: the order of the ladder (LRU
within a rung), stopping at the line instead of at zero, refusing only when the whole ladder cannot
free the build's estimate plus the reserve, and NEVER planning the current checkout below rung 6 (and
there never the tree being built), a live tree, a tracked file, `pkg/`, a tree that is not PlxNative's,
or another volume. The second half builds real
directories and a real git repository in a temp dir and checks the I/O: private-byte accounting under
hard links, the PlxNative marker, `/tmp` discovery, `origin/main` merged detection (squash merges, a
local `main` that is behind, and that detecting one writes nothing into the repository), and that
`report` leaves the disk exactly as it found it.
"""
import argparse
import contextlib
import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SCRIPT = os.path.join(ROOT, "tools", "disk-budget.py")

spec = importlib.util.spec_from_file_location("disk_budget", SCRIPT)
db = importlib.util.module_from_spec(spec)
sys.modules["disk_budget"] = db
spec.loader.exec_module(db)

GIB, MIB = db.GIB, db.MIB
NOW = 1_800_000_000.0
HOUR = 3600
ME = "/work/me"


def tree(path, kind="lane", **kw):
    """A synthetic tree. Sizes are in GiB, ages in hours ago; defaults are an idle, free-standing lane."""
    owner = kw.pop("owner", os.path.basename(os.path.dirname(os.path.dirname(path))) if kind == "lane" else "")
    private = int(kw.pop("gib", 1.0) * GIB)
    incr = int(kw.pop("incr", 0.0) * GIB)
    age = kw.pop("age", 5.0)
    if kind == "lane" and "checkout" not in kw:
        kw["checkout"] = os.path.dirname(os.path.dirname(path))
    return db.Tree(path=path, kind=kind, owner=owner, private_bytes=private, alloc_bytes=private,
                   incr_bytes=incr, incr_paths=(path + "/debug/incremental",) if incr else (),
                   last_built=NOW - age * HOUR, **kw)


def inv(*trees, **kw):
    kw.setdefault("protected_prefixes", ())
    kw.setdefault("cache_root", "/cache")
    return db.Inventory(tuple(trees), **kw)


def live(**kw):
    return db.Liveness(**kw)


TH = db.Thresholds()
FREE_LOW = 10 * GIB


def planned(pl):
    return [(a.rung, a.scope, a.tree) for a in pl.actions]


def run_plan(i, free=FREE_LOW, need=int(3.4 * GIB), me=ME, build="/work/me/rust-modules/target", th=TH):
    return db.plan(i, free, need, th, me, NOW, build)


class LadderOrder(unittest.TestCase):
    def setUp(self):
        self.i = inv(
            tree("/w/a/rust-modules/target", gib=4, age=3),                         # lane, 3h
            tree("/w/b/rust-modules/target", gib=4, age=9),                         # lane, 9h
            tree("/w/c/rust-modules/target", gib=3, incr=1, age=6),                 # lane with incremental
            tree("/tmp/old", kind="tmp", gib=2, age=50),                            # /tmp garbage
            tree("/tmp/young", kind="tmp", gib=2, age=2),                           # /tmp idle
            tree("/fleet/gone/target", kind="fleet", gib=1.5, age=1, orphan=True),  # orphan
            tree("/w/m/rust-modules/target", kind="lane", gib=2, age=12, checkout="/w/m", owner="m",
                 merged=True, clean=True),
        )

    def test_rungs_run_in_order_and_lru_within_a_rung(self):
        actions, _ = db.ladder(self.i, TH, ME, NOW)
        rungs = [a.rung for a in actions]
        self.assertEqual(rungs, sorted(rungs))
        zero = [a.tree for a in actions if a.rung == 0]
        # oldest built first: /tmp/old (50h), the merged lane (12h), the orphan (1h)
        self.assertEqual(zero, ["/tmp/old", "/w/m/rust-modules/target", "/fleet/gone/target"])
        four = [a.tree for a in actions if a.rung == 4]
        self.assertEqual(four, ["/w/b/rust-modules/target", "/w/c/rust-modules/target", "/w/a/rust-modules/target"])

    def test_incremental_dirs_come_before_whole_trees_and_the_whole_tree_frees_the_rest(self):
        actions, _ = db.ladder(self.i, TH, ME, NOW)
        c = [a for a in actions if a.tree == "/w/c/rust-modules/target"]
        self.assertEqual([(a.rung, a.scope) for a in c], [(2, "incremental"), (4, "tree")])
        self.assertEqual(c[0].frees, 1 * GIB)
        self.assertEqual(c[1].frees, 2 * GIB, "the whole-tree step must not count the incremental bytes twice")

    def test_young_tmp_tree_is_rung_3_and_between_incremental_and_whole_lanes(self):
        actions, _ = db.ladder(self.i, TH, ME, NOW)
        order = [(a.rung, a.tree) for a in actions]
        self.assertLess(order.index((3, "/tmp/young")), order.index((4, "/w/b/rust-modules/target")))
        self.assertLess(order.index((2, "/w/c/rust-modules/target")), order.index((3, "/tmp/young")))

    def test_plan_stops_at_the_line_not_at_zero(self):
        # 10 GiB free; goal = comfort 20 + hysteresis 4 = 24 GiB. Rung 0 frees 2 + 2 + 1.5 = 5.5, rung 2
        # 1, rung 3 2 -> 18.5; the first rung-4 action (b, 4 GiB) reaches 22.5 and the next (c, 2) 24.5.
        pl = run_plan(self.i)
        self.assertEqual(pl.verdict, "reclaim")
        self.assertGreaterEqual(pl.projected, 24 * GIB)
        self.assertNotIn((4, "tree", "/w/a/rust-modules/target"), planned(pl), "kept taking trees past the line")
        self.assertEqual(planned(pl)[-1], (4, "tree", "/w/c/rust-modules/target"))
        self.assertEqual(pl.projected, FREE_LOW + sum(a.frees for a in pl.actions))

    def test_nothing_to_do_above_the_comfort_line(self):
        pl = run_plan(self.i, free=30 * GIB)
        self.assertEqual(pl.verdict, "ok")
        self.assertEqual(pl.actions, ())

    def test_a_big_estimate_raises_the_bar_above_comfort(self):
        # 22 GiB free is over comfort, but a 20 GiB build plus the 3 GiB reserve is not covered.
        pl = run_plan(self.i, free=22 * GIB, need=20 * GIB)
        self.assertEqual(pl.verdict, "reclaim")
        self.assertGreaterEqual(pl.projected, 23 * GIB)

    def test_a_nearly_comfortable_machine_takes_only_what_it_needs(self):
        pl = run_plan(self.i, free=19 * GIB)             # 5 GiB short of 24
        self.assertEqual(pl.verdict, "reclaim")
        self.assertEqual({a.rung for a in pl.actions}, {0})   # rung 0 alone frees 5.5

    def test_below_comfort_with_nothing_eligible_is_ok_while_the_build_fits(self):
        pl = run_plan(inv(), free=12 * GIB)
        self.assertEqual((pl.verdict, pl.actions), ("ok", ()))
        self.assertIn("nothing is eligible", pl.reason)

    def test_plan_is_pure(self):
        a = run_plan(self.i)
        b = run_plan(self.i)
        self.assertEqual(a, b)

    def test_plan_does_no_io(self):
        real = {n: getattr(os, n) for n in ("stat", "lstat", "scandir", "listdir", "walk", "open")}
        real_run, real_open = subprocess.run, open

        def boom(*_a, **_k):
            raise AssertionError("the planner touched the machine")
        try:
            for n in real:
                setattr(os, n, boom)
            subprocess.run = boom
            import builtins
            builtins.open = boom
            pl = run_plan(self.i)
            db.ladder(self.i, TH, ME, NOW)
        finally:
            import builtins
            builtins.open = real_open
            subprocess.run = real_run
            for n, f in real.items():
                setattr(os, n, f)
        self.assertEqual(pl.verdict, "reclaim")


class NeverPlanned(unittest.TestCase):
    def kept(self, t, i=None, **kw):
        i = i or inv(t, **kw)
        actions, kept = db.ladder(i, TH, ME, NOW, "/work/me/rust-modules/target")
        return actions, kept.get(t.path, "")

    def test_the_current_checkout_is_never_planned_below_rung_6(self):
        t = tree("/work/me/rust-modules/target-fast", gib=3, incr=1, age=9, checkout=ME, owner="me")
        actions, why = self.kept(t)
        self.assertEqual({a.rung for a in actions}, {6})
        self.assertEqual([a.scope for a in actions], ["incremental"])
        self.assertIn("current checkout", why)

    def test_the_tree_being_built_is_never_planned_at_any_rung(self):
        t = tree("/work/me/rust-modules/target", gib=3, incr=1, age=9, checkout=ME, owner="me")
        actions, why = self.kept(t)
        self.assertEqual(actions, [])
        self.assertIn("being built", why)

    def test_a_live_current_tree_is_not_even_a_rung_6_candidate(self):
        t = tree("/work/me/rust-modules/target-fast", gib=3, incr=1, age=9, checkout=ME, owner="me",
                 live=live(open_files=("rustc",)))
        self.assertEqual(self.kept(t)[0], [])

    def test_make_running_in_the_current_checkout_does_not_protect_it_from_rung_6(self):
        # `make` is the caller: its cwd is always the current checkout. Open files / locks still count.
        t = tree("/work/me/rust-modules/target-fast", gib=3, incr=1, age=9, checkout=ME, owner="me",
                 live=live(builder_cwd="make", check_lock=True))
        self.assertEqual({a.rung for a in self.kept(t)[0]}, {6})

    def test_every_kind_of_liveness_protects_another_lane(self):
        for l in (live(builder_cwd="cargo"), live(open_files=("rustc",)), live(cargo_lock=True),
                  live(check_lock=True), live(unknown=True)):
            t = tree("/w/x/rust-modules/target", gib=4, age=9, live=l)
            actions, why = self.kept(t)
            self.assertEqual(actions, [], l)
            self.assertIn("in use", why)

    def test_a_live_merged_lane_or_orphan_is_not_deleted(self):
        t = tree("/fleet/gone/target", kind="fleet", gib=2, age=9, orphan=True, live=live(open_files=("cargo",)))
        self.assertEqual(self.kept(t)[0], [])

    def test_a_tree_with_a_tracked_file_is_never_planned(self):
        t = tree("/w/x/rust-modules/target", gib=4, age=9, tracked=True)
        actions, why = self.kept(t)
        self.assertEqual(actions, [])
        self.assertIn("tracks", why)

    def test_a_cargo_tree_without_a_plxnative_marker_is_never_planned_even_under_pressure(self):
        t = tree("/tmp/rs-matter-target", kind="tmp", gib=9, age=100, plx=False)
        pl = run_plan(inv(t), free=1 * GIB)
        self.assertEqual(pl.actions, ())
        self.assertEqual(pl.verdict, "refuse")
        self.assertIn("not ours", db.never(t, inv(t)))

    def test_pkg_vendor_and_the_ffmpeg_cache_are_never_planned(self):
        for path in ("/w/x/pkg/target", "/w/x/vendor/target", "/cache/ffmpeg/arm-9/target"):
            t = tree(path, kind="tmp", gib=4, age=100)
            i = inv(t, protected_prefixes=("/w/x/pkg", "/w/x/vendor", "/cache/ffmpeg"))
            self.assertTrue(db.never(t, i), path)
            self.assertEqual(db.ladder(i, TH, ME, NOW)[0], [], path)

    def test_a_tree_on_another_volume_is_never_planned(self):
        t = tree("/Volumes/x/target", kind="tmp", gib=4, age=100, dev=9)
        self.assertEqual(db.ladder(inv(t, volume_dev=1), TH, ME, NOW)[0], [])

    def test_the_planner_rechecks_what_the_inventory_should_have_filtered(self):
        bad = [tree("/t/a", kind="tmp", age=100, tracked=True), tree("/t/b", kind="tmp", age=100, plx=False),
               tree("/t/c", kind="tmp", age=100, protected="hand-marked")]
        pl = run_plan(inv(*bad), free=1 * GIB)
        self.assertEqual(pl.actions, ())

    def test_an_unclassified_kind_is_never_planned(self):
        self.assertTrue(db.never(tree("/t/a", kind="mystery", age=100), inv()))

    def test_a_freshly_built_lane_is_left_alone(self):
        t = tree("/w/x/rust-modules/target", gib=4, age=0.1)          # 6 minutes
        actions, why = self.kept(t)
        self.assertEqual(actions, [])
        self.assertIn("built", why)

    def test_a_lane_is_idle_only_when_all_of_its_trees_are(self):
        old = tree("/w/x/rust-modules/target", gib=4, age=9, checkout="/w/x", owner="x")
        busy = tree("/w/x/rust-modules/target-hostsim", gib=2, age=0.1, checkout="/w/x", owner="x")
        actions, kept = db.ladder(inv(old, busy), TH, ME, NOW)
        self.assertEqual(actions, [])
        self.assertIn("its lane built", kept[old.path])
        other = tree("/w/y/rust-modules/target", gib=4, age=9, checkout="/w/y", owner="y")
        self.assertEqual([a.tree for a in db.ladder(inv(old, busy, other), TH, ME, NOW)[0]], [other.path])

    def test_a_tree_that_shares_all_its_blocks_frees_nothing_and_is_not_planned(self):
        t = tree("/w/x/rust-modules/target", gib=0, age=9)
        actions, why = self.kept(t)
        self.assertEqual(actions, [])
        self.assertIn("frees nothing", why)

    def test_the_planner_uses_private_bytes_not_du(self):
        a = tree("/w/x/rust-modules/target", gib=0.2, age=9)
        a = db.Tree(**{**a.__dict__, "alloc_bytes": 40 * GIB})
        pl = run_plan(inv(a), free=1 * GIB)
        self.assertEqual(pl.projected, 1 * GIB + int(0.2 * GIB))


class MergedLanes(unittest.TestCase):
    def lane(self, **kw):
        kw = {"gib": 2, "age": 3, "checkout": "/w/m", "owner": "m", "merged": True, "clean": True, **kw}
        return tree("/w/m/rust-modules/target", **kw)

    def rung_of(self, t):
        actions, _ = db.ladder(inv(t), TH, ME, NOW)
        return [a.rung for a in actions]

    def test_merged_clean_unlocked_idle_lane_is_rung_0(self):
        self.assertEqual(self.rung_of(self.lane()), [0])

    def test_a_dirty_a_locked_or_a_busy_lane_is_not_garbage(self):
        for kw in ({"clean": False}, {"locked": True}, {"age": 0.1}):
            self.assertNotEqual(self.rung_of(self.lane(**kw))[:1], [0], kw)

    def test_an_unmerged_lane_waits_for_rung_4(self):
        self.assertEqual(self.rung_of(self.lane(merged=False)), [4])

    def test_an_orphan_is_rung_0_even_when_it_is_young_or_unmerged(self):
        t = tree("/fleet/gone/target", kind="fleet", gib=2, age=0.1, orphan=True)
        self.assertEqual(self.rung_of(t), [0])


class TmpTrees(unittest.TestCase):
    def test_age_picks_the_rung(self):
        old = tree("/tmp/o", kind="tmp", age=25)
        mid = tree("/tmp/m", kind="tmp", age=1)
        new = tree("/tmp/n", kind="tmp", age=0.1)
        actions, kept = db.ladder(inv(old, mid, new), TH, ME, NOW)
        self.assertEqual({(a.tree, a.rung) for a in actions}, {("/tmp/o", 0), ("/tmp/m", 3)})
        self.assertIn("built", kept["/tmp/n"])


class TombstonesAndSeed(unittest.TestCase):
    def test_a_tombstone_is_rung_0_unless_a_process_holds_it(self):
        t = db.Tree("/cache/ffmpeg/x.tombstone.7", "leftover", private_bytes=GIB, last_built=NOW, tombstone=True)
        i = inv(t, protected_prefixes=("/cache/ffmpeg",))      # the FFmpeg cache is protected; its tombstones are not
        self.assertEqual([a.rung for a in db.ladder(i, TH, ME, NOW)[0]], [0])
        held = db.Tree(**{**t.__dict__, "live": live(open_files=("rm",))})
        self.assertEqual(db.ladder(inv(held, protected_prefixes=("/cache/ffmpeg",)), TH, ME, NOW)[0], [])

    def test_a_tombstone_outside_the_build_cache_is_refused(self):
        t = db.Tree("/elsewhere/x.tombstone.7", "leftover", private_bytes=GIB, last_built=NOW, tombstone=True)
        self.assertTrue(db.never(t, inv(t)))

    def test_the_cargo_seed_is_never_planned_at_any_age(self):
        # Lanes cloned from the seed share its blocks (measured 12K private against 478M by `du`): deleting it
        # frees almost nothing and every new lane's first build is cold. `build-gc.sh --seed` is the manual prune.
        for days in (3, 9, 400):
            with self.subTest(days=days):
                seed = db.Tree("/cache/cargo-seed/target", "seed", private_bytes=GIB, last_built=NOW - days * 86400)
                actions, kept = db.ladder(inv(seed), TH, ME, NOW)
                self.assertEqual(actions, [])
                self.assertIn("shares its blocks", kept["/cache/cargo-seed/target"])
                pl = run_plan(inv(seed), free=1 * GIB)
                self.assertEqual(pl.actions, ())

    def test_a_locked_seed_stays(self):
        old = db.Tree("/cache/cargo-seed/target", "seed", private_bytes=GIB, last_built=NOW - 9 * 86400,
                      live=live(cargo_lock=True))
        self.assertEqual(db.ladder(inv(old), TH, ME, NOW)[0], [])


class MainCheckout(unittest.TestCase):
    def main_tree(self, **kw):
        return tree("/main/rust-modules/target", kind="main", checkout="/main", owner="main", gib=6, age=9, **kw)

    def test_main_incremental_is_rung_6_and_its_stale_sweep_is_rung_5_unsized(self):
        actions, _ = db.ladder(inv(self.main_tree(incr=2)), TH, ME, NOW)
        self.assertEqual(sorted((a.rung, a.scope, a.sized) for a in actions),
                         [(5, "stale", False), (6, "incremental", True)])

    def test_a_live_main_checkout_is_untouched(self):
        t = self.main_tree(incr=2, live=live(builder_cwd="cargo"))
        self.assertEqual(db.ladder(inv(t), TH, ME, NOW)[0], [])

    def test_main_is_not_planned_while_rungs_0_to_5_can_do_the_job(self):
        i = inv(self.main_tree(incr=2), tree("/w/a/rust-modules/target", gib=30, age=9))
        pl = run_plan(i)
        self.assertNotIn(6, {a.rung for a in pl.actions})
        self.assertNotIn("incremental", {a.scope for a in pl.actions if a.tree.startswith("/main")})

    def test_the_stale_sweep_makes_the_plan_uncertain(self):
        pl = run_plan(inv(self.main_tree(incr=0)))
        self.assertTrue(pl.uncertain)
        self.assertEqual(pl.verdict, "reclaim")


class Rung6AndRefusal(unittest.TestCase):
    def setUp(self):
        self.main = tree("/main/rust-modules/target", kind="main", checkout="/main", owner="main", gib=6, incr=2.5, age=9)
        self.mine = tree("/work/me/rust-modules/target-fast", gib=3, incr=1.5, age=9, checkout=ME, owner="me")
        self.building = tree("/work/me/rust-modules/target", gib=5, incr=3, age=1, checkout=ME, owner="me")
        self.small = tree("/w/a/rust-modules/target", gib=1, age=9)

    def test_rung_6_is_taken_only_when_the_build_would_be_refused(self):
        i = inv(self.main, self.mine, self.building, self.small)
        # 8 GiB free, 3.4 build + 3 reserve = 6.4 needed: comfortable enough to build, below comfort to reclaim.
        pl = run_plan(i, free=8 * GIB)
        self.assertNotIn(6, {a.rung for a in pl.actions})
        # 2 GiB free: even rungs 0-5 (1 GiB) cannot reach 6.4, so rung 6 goes: main first, then the current checkout.
        pl = run_plan(i, free=2 * GIB)
        r6 = [a for a in pl.actions if a.rung == 6]
        self.assertEqual([a.tree for a in r6], ["/main/rust-modules/target", "/work/me/rust-modules/target-fast"])
        self.assertEqual(pl.verdict, "reclaim")
        self.assertGreaterEqual(pl.projected, int(6.4 * GIB))

    def test_rung_6_stops_at_the_build_need_not_at_comfort(self):
        i = inv(self.main, self.mine, self.building, self.small)
        pl = run_plan(i, free=4 * GIB)          # 4 + 1 (rung 4) = 5 < 6.4; main incremental (2.5) reaches 7.5
        r6 = [a.tree for a in pl.actions if a.rung == 6]
        self.assertEqual(r6, ["/main/rust-modules/target"])

    def test_the_tree_being_built_is_never_in_rung_6(self):
        i = inv(self.main, self.mine, self.building, self.small)
        pl = run_plan(i, free=0)
        self.assertNotIn("/work/me/rust-modules/target", {a.tree for a in pl.actions})

    def test_refuses_when_the_whole_ladder_cannot_cover_the_estimate_plus_the_reserve(self):
        i = inv(self.mine, self.building, self.small)
        pl = run_plan(i, free=1 * GIB, need=20 * GIB)
        self.assertEqual(pl.verdict, "refuse")
        self.assertIn("cannot free enough", pl.reason)
        self.assertIn("23.0 GiB", pl.reason)            # need + reserve is in the reason

    def test_an_unsized_stale_sweep_counts_as_zero_so_a_refusal_stays_reachable(self):
        # The main checkout's `--stale` sweep cannot be sized ahead of time. Treating it as "might free enough"
        # made a refusal unreachable whenever a sweep was in the plan, so it counts as 0
        # in the verdict: the plan still carries the sweep, in its rung-5 slot, and `settle` judges the free
        # space `build-gc.sh --evict` re-reads after running it.
        i = inv(self.main, self.mine, self.building, self.small)
        pl = run_plan(i, free=1 * GIB, need=20 * GIB)
        self.assertEqual(pl.verdict, "refuse")
        self.assertTrue(pl.uncertain)
        self.assertIn((5, "stale"), {(a.rung, a.scope) for a in pl.actions})
        self.assertLess(pl.projected, 23 * GIB)
        self.assertIn("--stale", pl.reason)
        self.assertIn("settle", pl.reason)

    def test_settle_judges_the_free_space_read_after_the_plan_ran(self):
        need, reserve = int(3.4 * GIB), 3 * GIB
        self.assertEqual(db.settle(need, reserve, int(6.4 * GIB))[0], "ok")
        verdict, why = db.settle(need, reserve, int(6.3 * GIB))
        self.assertEqual(verdict, "refuse")
        self.assertIn("6.4 GiB", why)
        self.assertEqual(db.settle(0, reserve, 3 * GIB)[0], "ok")

    def test_refusal_names_the_biggest_live_trees(self):
        busy = tree("/w/busy/rust-modules/target", gib=9, age=9, live=live(builder_cwd="cargo"))
        pl = run_plan(inv(busy, self.small), free=1 * GIB, need=20 * GIB)
        self.assertEqual(pl.verdict, "refuse")
        self.assertEqual(pl.biggest_live[0][0], "/w/busy/rust-modules/target")
        self.assertIn("cargo", pl.biggest_live[0][2])

    def test_it_does_not_refuse_when_the_ladder_just_covers_the_need(self):
        t = tree("/w/a/rust-modules/target", gib=5.4, age=9)           # 1 + 5.4 = 6.4 = 3.4 + 3
        pl = run_plan(inv(t), free=1 * GIB)
        self.assertEqual(pl.verdict, "reclaim")
        t = tree("/w/a/rust-modules/target", gib=5.3, age=9)
        self.assertEqual(run_plan(inv(t), free=1 * GIB).verdict, "refuse")

    def test_an_empty_inventory_under_pressure_refuses_with_a_reason(self):
        pl = run_plan(inv(), free=1 * GIB)
        self.assertEqual(pl.verdict, "refuse")
        self.assertTrue(pl.reason)


class Estimates(unittest.TestCase):
    def test_need_is_the_estimate_minus_what_the_tree_holds_with_a_floor(self):
        self.assertEqual(db.estimate_need("/x/target", 0, {}), int(3.4 * GIB))
        self.assertEqual(db.estimate_need("/x/target", int(3 * GIB), {}), int(0.5 * GIB))
        self.assertEqual(db.estimate_need("/x/target-storage", 0, {}), int(0.5 * GIB))
        self.assertEqual(db.estimate_need("/x/target-hostsim", int(1 * GIB), {}), int(2.5 * GIB))
        self.assertEqual(db.estimate_need("/x/target-unknown", 0, {}), int(3.4 * GIB))

    def test_the_environment_overrides_the_estimate(self):
        self.assertEqual(db.estimate_need("/x/target", 0, {"PLX_DISK_NEED_GIB": "8"}), 8 * GIB)
        self.assertEqual(db.estimate_need("/x/target", 0, {"PLX_DISK_NEED_GIB": "junk"}), int(3.4 * GIB))

    def test_thresholds_come_from_the_environment_build_gc_already_reads(self):
        th = db.thresholds_from_env({"PLX_GC_MIN_FREE_GIB": "30", "PLX_MIN_FREE_MB": "1024"})
        self.assertEqual((th.comfort, th.reserve), (30 * GIB, 1024 * MIB))
        self.assertEqual(th.hysteresis, 4 * GIB)
        d = db.thresholds_from_env({})
        self.assertEqual((d.comfort, d.hysteresis, d.reserve), (20 * GIB, 4 * GIB, 3 * GIB))

    def test_test_free_override_matches_build_gc(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(db.free_bytes(d, {"PLX_GC_TEST_FREE_KIB": "2048"})[0], 2048 * 1024)


# ---------------------------------------------------------------------------------------------------
# The inventory: real directories and a real git repository.
# ---------------------------------------------------------------------------------------------------
def write(path, size=64 * 1024, fill=b"x"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(os.urandom(size) if fill is None else fill * size)


def age_all(path, hours):
    """Set every mtime under `path` (files, dirs, the dir itself) `hours` ago."""
    t = NOW_REAL - hours * HOUR
    for dirpath, dirs, files in os.walk(path, topdown=False):
        for n in files + dirs:
            try:
                os.utime(os.path.join(dirpath, n), (t, t), follow_symlinks=False)
            except OSError:
                pass
    os.utime(path, (t, t))


NOW_REAL = __import__("time").time()


def cargo_tree(path, ours=True, incr=0, size=64 * 1024, hours=5):
    """A cargo target dir: CACHEDIR.TAG, debug/{deps,.fingerprint,incremental}, PlxNative-named artifacts."""
    prefix = "libplx_base" if ours else "librs_matter"
    write(os.path.join(path, "CACHEDIR.TAG"), 16)
    write(os.path.join(path, "debug", "deps", prefix + "-aaaa.rlib"), size, None)
    write(os.path.join(path, "debug", "deps", prefix + "-aaaa.d"), 4096, None)
    write(os.path.join(path, "debug", ".fingerprint", prefix + "-aaaa", "lib"), 4096, None)
    for i in range(incr):
        write(os.path.join(path, "debug", "incremental", "plx_ui-x", "s-%d" % i, "q.bin"), size, None)
    age_all(path, hours)


class QuietProbes(db.Probes):
    """No lsof, no locks: every tree reads idle, and a test sets what it wants live."""

    def __init__(self, builders=(), files=(), cargo=(), check=(), unknown=False):
        super().__init__({})
        self.fake = db.LiveSnapshot(list(builders), list(files), unknown)
        self.cargo, self.check = set(cargo), set(check)

    def snapshot(self):
        return self.fake

    def cargo_locked(self, tree):
        return tree in self.cargo

    def check_locked(self, checkout):
        return checkout in self.check

    def seed_locked(self, seed):
        return False


def git(cwd, *args):
    out = subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c",
                          "commit.gpgsign=false", "-c", "protocol.file.allow=always", "-C", cwd, *args],
                         capture_output=True, text=True, env=clean_env())
    if out.returncode:
        raise AssertionError("git %s failed: %s%s" % (" ".join(args), out.stdout, out.stderr))
    return out.stdout.strip()


def clean_env():
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return env


def git_has_merge_tree():
    """`git merge-tree --write-tree` (the squash check) arrived in git 2.38."""
    import re
    out = subprocess.run(["git", "--version"], capture_output=True, text=True).stdout
    m = re.search(r"(\d+)\.(\d+)", out)
    return bool(m) and (int(m.group(1)), int(m.group(2))) >= (2, 38)


def changed(before, after):
    """The paths that differ between two snapshot_files() results, for a readable failure."""
    return sorted(k for k in set(before) | set(after) if before.get(k) != after.get(k))


def snapshot_files(root):
    """{path: (size, mtime_ns)} for everything under `root`. Inside a `.git` directory git is allowed to
    refresh an existing file's mtime (it freshens an object it would otherwise have to write again), so
    there only the names and sizes count: no object is added, removed or changed."""
    out = {}
    for dirpath, dirs, files in os.walk(root):
        in_git = ("%s.git%s" % (os.sep, os.sep)) in (dirpath + os.sep) or os.path.basename(dirpath) == ".git"
        for n in files + dirs:
            p = os.path.join(dirpath, n)
            st = os.lstat(p)
            out[p] = (st.st_size,) if in_git else (st.st_size, st.st_mtime_ns)
    return out


class Repo(unittest.TestCase):
    """origin.git + a main checkout with a stale local `main` + lanes, in a temp dir."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="disk-budget-test-")
        self.addCleanup(tmp.cleanup)
        self.tmp = os.path.realpath(tmp.name)
        self.origin = os.path.join(self.tmp, "origin.git")
        self.main = os.path.join(self.tmp, "main")
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", self.origin], check=True, env=clean_env())
        subprocess.run(["git", "clone", "-q", self.origin, self.main], check=True, capture_output=True,
                       env=clean_env())
        git(self.main, "checkout", "-q", "-B", "main")
        write(os.path.join(self.main, "README"), 10)
        write(os.path.join(self.main, "src.txt"), 10)
        with open(os.path.join(self.main, ".gitignore"), "w") as fh:
            fh.write("target*/\npkg/\n")                 # as the real repository ignores its build output
        git(self.main, "add", "README", "src.txt", ".gitignore")
        git(self.main, "commit", "-q", "-m", "base")
        git(self.main, "push", "-q", "-u", "origin", "main")
        self.fleet = os.path.join(self.tmp, "fleet")
        self.cache = os.path.join(self.tmp, "cache")
        self.tmpr = os.path.join(self.tmp, "tmproot")
        for d in (self.fleet, self.cache, self.tmpr):
            os.makedirs(d)
        self.env = {"PLX_FLEET_DIR": self.fleet, "PLX_BUILD_CACHE": self.cache, "PLX_GC_TMP_ROOTS": self.tmpr}

    def lane(self, name, files=None):
        path = os.path.join(self.tmp, name)
        git(self.main, "worktree", "add", "-q", "-b", name, path)
        for rel, text in (files or {}).items():
            write(os.path.join(path, rel), 20, text.encode()[:1])
            git(path, "add", rel)
            git(path, "commit", "-q", "-m", name + " " + rel)
        return path

    def squash_into_origin(self, lane):
        """Land `lane` on origin/main as ONE squash commit, then leave the local `main` one commit behind."""
        git(self.main, "merge", "--squash", os.path.basename(lane))
        git(self.main, "commit", "-q", "-m", "squash " + lane)
        git(self.main, "push", "-q", "origin", "main")
        git(self.main, "reset", "-q", "--hard", "HEAD~1")

    def collect(self, probes=None, root=None):
        return db.collect(root or self.main, self.env, probes or QuietProbes(), NOW_REAL, size_foreign=True)

    def by_path(self, i):
        return {t.path: t for t in i.trees}


@unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
class MergedDetection(Repo):
    def test_squash_merged_lane_reads_merged_against_origin_main_while_local_main_is_behind(self):
        lane = self.lane("squashed", {"a.txt": "a"})
        self.squash_into_origin(lane)
        self.assertEqual(db.base_ref(self.main), "refs/remotes/origin/main")
        sha = git(lane, "rev-parse", "HEAD")
        self.assertTrue(db.is_merged(lane, sha, "refs/remotes/origin/main"))
        self.assertFalse(db.is_merged(lane, sha, "refs/heads/main"), "the stale local main must not count")

    def test_a_lane_with_unlanded_work_is_not_merged(self):
        lane = self.lane("open", {"b.txt": "b"})
        self.assertFalse(db.is_merged(lane, git(lane, "rev-parse", "HEAD"), "refs/remotes/origin/main"))

    def test_a_lane_with_no_commits_reads_merged_but_only_its_tree_is_ever_planned(self):
        lane = self.lane("fresh")
        self.assertTrue(db.is_merged(lane, git(lane, "rev-parse", "HEAD"), "refs/remotes/origin/main"))

    def test_detecting_a_squash_writes_nothing_into_the_repository(self):
        lane = self.lane("squashed", {"a.txt": "a"})
        self.squash_into_origin(lane)
        before = snapshot_files(os.path.join(self.main, ".git", "objects"))
        self.assertTrue(db.is_merged(lane, git(lane, "rev-parse", "HEAD"), "refs/remotes/origin/main"))
        self.assertEqual(changed(before, snapshot_files(os.path.join(self.main, ".git", "objects"))), [])

    def test_detecting_an_unlanded_lane_adds_no_object_either(self):
        lane = self.lane("open", {"b.txt": "b"})
        write(os.path.join(self.main, "other.txt"), 10)          # main moved on: the merge result is a NEW tree
        git(self.main, "add", "other.txt")
        git(self.main, "commit", "-q", "-m", "main moves")
        git(self.main, "push", "-q", "origin", "main")
        objects = os.path.join(self.main, ".git", "objects")
        before = snapshot_files(objects)
        self.assertFalse(db.is_merged(lane, git(lane, "rev-parse", "HEAD"), "refs/remotes/origin/main"))
        self.assertEqual(changed(before, snapshot_files(objects)), [])

    def test_base_ref_falls_back_to_local_main_and_to_nothing(self):
        git(self.main, "remote", "remove", "origin")
        self.assertEqual(db.base_ref(self.main), "refs/heads/main")
        git(self.main, "branch", "-m", "main", "trunk")
        self.assertEqual(db.base_ref(self.main), "")
        self.assertFalse(db.is_merged(self.main, "deadbeef", ""))


class Measure(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="disk-budget-test-")
        self.addCleanup(tmp.cleanup)
        self.tmp = os.path.realpath(tmp.name)

    def test_private_bytes_count_each_inode_once_and_an_unshared_file_in_full(self):
        t = os.path.join(self.tmp, "t")
        write(os.path.join(t, "debug", "deps", "a"), 256 * 1024, None)
        write(os.path.join(t, "debug", "deps", "b"), 256 * 1024, None)
        os.link(os.path.join(t, "debug", "deps", "a"), os.path.join(t, "debug", "deps", "a-link"))
        private, alloc, incr = db.measure(t, ())
        self.assertGreaterEqual(private, 2 * 256 * 1024)
        self.assertLess(private, 3 * 256 * 1024, "a hard link was counted twice")
        self.assertEqual(incr, 0)
        self.assertGreaterEqual(alloc, private)

    def test_a_file_hard_linked_out_of_the_tree_is_not_freed_by_deleting_it(self):
        t, outside = os.path.join(self.tmp, "t"), os.path.join(self.tmp, "outside")
        write(os.path.join(t, "debug", "deps", "a"), 256 * 1024, None)
        write(os.path.join(t, "debug", "deps", "b"), 256 * 1024, None)
        os.makedirs(outside)
        os.link(os.path.join(t, "debug", "deps", "a"), os.path.join(outside, "a"))
        private, _, _ = db.measure(t, ())
        self.assertGreaterEqual(private, 256 * 1024)
        self.assertLess(private, 2 * 256 * 1024, "an inode with a name outside the tree was counted as freed")

    def test_incremental_work_products_hard_linked_into_deps_are_not_freed_by_removing_incremental_alone(self):
        t = os.path.join(self.tmp, "t")
        incr = os.path.join(t, "debug", "incremental")
        write(os.path.join(incr, "plx-x", "own.bin"), 256 * 1024, None)
        write(os.path.join(incr, "plx-x", "linked.o"), 256 * 1024, None)
        os.makedirs(os.path.join(t, "debug", "deps"))
        os.link(os.path.join(incr, "plx-x", "linked.o"), os.path.join(t, "debug", "deps", "linked.o"))
        private, _, incr_private = db.measure(t, (incr,))
        self.assertGreaterEqual(private, 2 * 256 * 1024)
        self.assertGreaterEqual(incr_private, 256 * 1024)
        self.assertLess(incr_private, 2 * 256 * 1024, "deleting incremental/ cannot free the file deps/ still links")

    def test_symlinks_and_directories_are_not_followed(self):
        t, big = os.path.join(self.tmp, "t"), os.path.join(self.tmp, "big")
        write(os.path.join(big, "huge"), 1024 * 1024, None)
        write(os.path.join(t, "debug", "deps", "a"), 4096, None)
        os.symlink(big, os.path.join(t, "debug", "deps", "link"))
        private, alloc, _ = db.measure(t, ())
        self.assertLess(alloc, 512 * 1024)
        self.assertLess(private, 512 * 1024)


class Inventory(Repo):
    def test_marker_and_discovery(self):
        ours, theirs = os.path.join(self.tmpr, "ours"), os.path.join(self.tmpr, "deep", "er", "theirs")
        cargo_tree(ours)
        cargo_tree(theirs, ours=False)
        self.assertTrue(db.has_plx_marker(ours))
        self.assertFalse(db.has_plx_marker(theirs), "a CACHEDIR.TAG tree of another project is not ours")
        self.assertEqual(db.discover_tmp_trees([self.tmpr], []), sorted([ours, theirs]))
        self.assertEqual(db.discover_tmp_trees([self.tmpr], [theirs]), [ours])

    def test_discovery_stops_at_depth_and_at_a_tagged_tree(self):
        deep = os.path.join(self.tmpr, "a", "b", "c", "d", "e")
        cargo_tree(deep)
        self.assertEqual(db.discover_tmp_trees([self.tmpr], []), [])
        top = os.path.join(self.tmpr, "t")
        cargo_tree(top)
        write(os.path.join(top, "debug", "nested", "CACHEDIR.TAG"), 4)
        self.assertEqual(db.discover_tmp_trees([self.tmpr], []), [top])

    def test_a_tagged_tree_that_holds_a_checkout_is_not_a_tmp_tree(self):
        # Discovery never ENTERS a registered checkout, but a CACHEDIR.TAG directory ABOVE one (a target dir
        # someone made a worktree inside) would be listed, and deleting it would delete the checkout: the git
        # tracked-file probe cannot see a repository nested below the tree it is asked about.
        holder = os.path.join(self.tmpr, "holder")
        cargo_tree(holder)
        checkout = os.path.join(holder, "lane")
        os.makedirs(checkout)
        self.assertEqual(db.discover_tmp_trees([self.tmpr], []), [holder])
        self.assertEqual(db.discover_tmp_trees([self.tmpr], [checkout]), [])

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for the squash check")
    def test_collect_classifies_every_kind_and_writes_nothing(self):
        merged = self.lane("merged-lane", {"m.txt": "m"})
        self.squash_into_origin(merged)
        busy = self.lane("busy-lane", {"n.txt": "n"})
        fresh = self.lane("fresh-lane")
        cargo_tree(os.path.join(self.main, "rust-modules", "target"), incr=1, hours=40)
        cargo_tree(os.path.join(merged, "rust-modules", "target"), hours=6)
        cargo_tree(os.path.join(busy, "rust-modules", "target"), hours=6)
        cargo_tree(os.path.join(busy, "rust-modules", "target-fast"), hours=6)
        cargo_tree(os.path.join(fresh, "rust-modules", "target"), hours=6)
        cargo_tree(os.path.join(self.fleet, "busy-lane", "target"), hours=6)
        cargo_tree(os.path.join(self.fleet, "gone-lane", "target"), hours=6)
        cargo_tree(os.path.join(self.tmpr, "ours"), hours=30)
        cargo_tree(os.path.join(self.tmpr, "theirs", "t"), ours=False, hours=30)
        seed = os.path.join(self.cache, "cargo-seed", "target")
        cargo_tree(seed, hours=24 * 9)
        write(os.path.join(seed, ".last-used"), 1)
        os.utime(os.path.join(seed, ".last-used"), (NOW_REAL - 9 * 86400,) * 2)
        cargo_tree(os.path.join(self.cache, "cargo-seed", "target.tmp.99"), hours=48)
        cargo_tree(os.path.join(self.cache, "ffmpeg", "arm-9.tombstone.123"), hours=1)
        write(os.path.join(self.cache, "ffmpeg", "arm-9-live", "libavcodec.a"), 4096)
        # the checkouts' pkg/ and vendor/ hold trees too; they are not derived from cargo
        write(os.path.join(self.main, "pkg", "dist", "x"), 4096)

        before = snapshot_files(self.tmp)
        i = self.collect(QuietProbes(builders=[("cargo", busy)], cargo=[os.path.join(self.fleet, "gone-lane", "target")]))
        self.assertEqual(changed(before, snapshot_files(self.tmp)), [], "collecting an inventory changed the disk")
        t = self.by_path(i)

        main_t = t[os.path.join(self.main, "rust-modules", "target")]
        self.assertEqual((main_t.kind, main_t.owner, main_t.merged), ("main", "main", False))
        self.assertGreater(main_t.incr_bytes, 0)
        self.assertGreater(main_t.private_bytes, main_t.incr_bytes)

        m = t[os.path.join(merged, "rust-modules", "target")]
        self.assertEqual((m.kind, m.owner, m.merged, m.clean, m.locked), ("lane", "merged-lane", True, True, False))
        self.assertTrue(i.base_ref.endswith("origin/main"))

        b = t[os.path.join(busy, "rust-modules", "target")]
        self.assertFalse(b.merged)
        self.assertEqual(b.live.builder_cwd, "cargo", "a builder in the checkout makes all of its trees live")
        self.assertEqual(t[os.path.join(busy, "rust-modules", "target-fast")].live.builder_cwd, "cargo")
        self.assertEqual(t[os.path.join(self.fleet, "busy-lane", "target")].live.builder_cwd, "cargo",
                         "a fleet tree is live while its lane's checkout is building")
        self.assertFalse(t[os.path.join(fresh, "rust-modules", "target")].live.reasons())

        f_orphan = t[os.path.join(self.fleet, "gone-lane", "target")]
        self.assertEqual((f_orphan.kind, f_orphan.orphan, f_orphan.live.cargo_lock), ("fleet", True, True))
        self.assertFalse(t[os.path.join(self.fleet, "busy-lane", "target")].orphan)

        ours = t[os.path.join(self.tmpr, "ours")]
        self.assertEqual((ours.kind, ours.plx), ("tmp", True))
        theirs = t[os.path.join(self.tmpr, "theirs", "t")]
        self.assertFalse(theirs.plx)
        self.assertEqual((theirs.private_bytes, theirs.incr_bytes), (0, 0))

        s = t[seed]
        self.assertEqual(s.kind, "seed")
        self.assertAlmostEqual(s.last_built, NOW_REAL - 9 * 86400, delta=5)
        self.assertEqual(t[os.path.join(self.cache, "cargo-seed", "target.tmp.99")].kind, "leftover")
        tomb = t[os.path.join(self.cache, "ffmpeg", "arm-9.tombstone.123")]
        self.assertEqual((tomb.kind, tomb.tombstone), ("leftover", True))
        self.assertNotIn(os.path.join(self.cache, "ffmpeg", "arm-9-live"), t, "live FFmpeg cache entries are not ours")
        self.assertFalse([p for p in t if "/pkg/" in p])

        # ... and what the planner makes of it, end to end.
        pl = db.plan(i, 2 * GIB, int(3.4 * GIB), db.Thresholds(), self.main, NOW_REAL)
        names = {os.path.relpath(a.tree, self.tmp) for a in pl.actions}
        self.assertNotIn(os.path.join("busy-lane", "rust-modules", "target"), names)
        self.assertNotIn(os.path.join("fleet", "busy-lane", "target"), names)
        self.assertNotIn(os.path.join("fleet", "gone-lane", "target"), names, "a held cargo lock protects an orphan")
        self.assertNotIn(os.path.join("tmproot", "theirs", "t"), names)
        self.assertIn(os.path.join("merged-lane", "rust-modules", "target"), names)
        self.assertEqual(db.ladder(i, db.Thresholds(), self.main, NOW_REAL)[0][0].rung, 0)

    def test_a_tree_with_a_tracked_file_is_marked_and_never_planned(self):
        lane = self.lane("tracked-lane")
        tgt = os.path.join(lane, "rust-modules", "target")
        cargo_tree(tgt, hours=9)
        git(lane, "add", "-f", os.path.join("rust-modules", "target", "CACHEDIR.TAG"))
        t = self.by_path(self.collect(root=self.main))[tgt]
        self.assertTrue(t.tracked)
        i = db.Inventory((t,), cache_root=self.cache)
        self.assertEqual(db.ladder(i, db.Thresholds(), self.main, NOW_REAL)[0], [])

    def test_the_current_checkout_is_kept_while_make_runs_in_it(self):
        lane = self.lane("mine")
        cargo_tree(os.path.join(lane, "rust-modules", "target"), hours=9)
        cargo_tree(os.path.join(lane, "rust-modules", "target-fast"), incr=1, hours=9)
        i = self.collect(QuietProbes(builders=[("make", lane)]), root=lane)
        actions, kept = db.ladder(i, db.Thresholds(), lane, NOW_REAL, os.path.join(lane, "rust-modules", "target"))
        self.assertEqual({a.rung for a in actions}, {6})
        self.assertEqual({os.path.basename(a.tree) for a in actions}, {"target-fast"})

    def test_an_lsof_that_printed_nothing_is_unknown_not_idle(self):
        good = "p100\ncmake\nfcwd\nn/work/a\n"
        self.assertFalse(db.interpret_lsof(0, good).unknown)
        self.assertFalse(db.interpret_lsof(1, good).unknown, "macOS lsof exits 1 and still lists the rest")
        self.assertEqual(db.interpret_lsof(1, good).builders, [("make", "/work/a")])
        for rc, text in ((1, ""), (0, ""), (1, "\n"), (2, good), (127, good)):
            self.assertTrue(db.interpret_lsof(rc, text).unknown, (rc, text))

    def test_lsof_missing_makes_everything_live(self):
        lane = self.lane("other")
        cargo_tree(os.path.join(lane, "rust-modules", "target"), hours=9)
        i = self.collect(QuietProbes(unknown=True))
        for t in i.trees:
            self.assertTrue(t.live.unknown)
        pl = db.plan(i, 1 * GIB, int(3.4 * GIB), db.Thresholds(), self.main, NOW_REAL)
        self.assertEqual(pl.actions, ())

    def test_parse_lsof_builds_the_builder_and_open_file_lists(self):
        text = "\n".join(["p100", "cmake", "fcwd", "n/work/a", "p200", "ccargo", "fcwd", "n/work/b/rust-modules",
                          "f5", "n/work/b/rust-modules/target/debug/.cargo-lock", "frtd", "n/",
                          "p300", "cmake", "fcwd", "n/work/c", "p400", "cvim", "fcwd", "n/work/d",
                          "f7", "n/work/d/notes"])
        snap = db.parse_lsof(text, ignore_pids=[300])
        self.assertEqual(snap.builders, [("make", "/work/a"), ("cargo", "/work/b/rust-modules")])
        self.assertIn(("cargo", "/work/b/rust-modules/target/debug/.cargo-lock"), snap.files)
        self.assertIn(("vim", "/work/d/notes"), snap.files)
        self.assertNotIn("/", [n for _, n in snap.files])

    def test_live_for_attributes_a_builder_to_its_checkout_and_to_a_fleet_lane(self):
        snap = db.LiveSnapshot([("cargo", "/c/lane-a/rust-modules")], [("rustc", "/t/x/debug/deps/o")])
        l = db.live_for("/c/lane-a/rust-modules/target", "/c/lane-a", None, snap, False, False, ["/c", "/c/lane-a"])
        self.assertEqual(l.builder_cwd, "cargo")
        self.assertFalse(db.live_for("/c/rust-modules/target", "/c", None, snap, False, False, ["/c", "/c/lane-a"]).builder_cwd)
        self.assertEqual(db.live_for("/fleet/lane-a/target", None, "lane-a", snap, False, False, ["/c"]).builder_cwd, "cargo")
        self.assertEqual(db.live_for("/t/x", None, None, snap, False, False).open_files, ("rustc",))
        self.assertTrue(db.live_for("/t/y", None, None, snap, True, False).cargo_lock)


class OtherVolume(Repo):
    """A tree on another volume than the build (a lane worktree on a slow USB disk): the inventory lists it,
    and nothing more. Sizing it, asking git about its lane or probing its locks reads a disk the build never
    writes to, which stalled the preflight for 20-35 minutes on a spinning one, and `never()` throws the
    tree out afterwards anyway. A second real device is impractical in CI, so the one device lookup
    (`device_of`) is stubbed, and every reader the inventory must NOT reach for such a tree raises."""

    OTHER = 4242

    def foreign(self, *prefixes, check=()):
        """Stub the device lookup so everything under `prefixes` is another volume, and make every reader
        of such a tree raise: a sizing walk, an mtime walk, a git probe, a cargo-lock probe. The `make check`
        lock probe is the one reader that is allowed: it flocks a file in the local lock directory named by a
        hash of the checkout's path and reads nothing on the checkout's own disk, so a probe of a foreign
        checkout is recorded in `foreign_lock_probes` rather than refused. `check` are the checkouts whose
        lock reads as held."""
        real_device_of = db.device_of
        self.other = prefixes
        self.foreign_lock_probes = []

        def is_other(path):
            return any(db.under(path, p) for p in prefixes)

        def device_of(path):
            return self.OTHER if is_other(path) else real_device_of(path)

        stubs = {"device_of": device_of}
        for name in ("measure", "newest_mtime", "tracked_under", "is_merged", "is_clean"):
            def guard(path, *args, _orig=getattr(db, name), _name=name, **kw):
                if is_other(path):
                    raise AssertionError("%s read a tree on another volume: %s" % (_name, path))
                return _orig(path, *args, **kw)
            stubs[name] = guard
        for name, stub in stubs.items():
            patcher = mock.patch.object(db, name, stub)
            patcher.start()
            self.addCleanup(patcher.stop)

        class Probes(QuietProbes):
            def cargo_locked(probes, path):
                if is_other(path):
                    raise AssertionError("a lock probe reached a tree on another volume: %s" % path)
                return super().cargo_locked(path)

            def check_locked(probes, checkout):
                if is_other(checkout):
                    self.foreign_lock_probes.append(checkout)
                return super().check_locked(checkout)

        return Probes(check=check)

    def test_a_lane_on_another_volume_is_listed_but_never_sized_probed_or_planned(self):
        das = self.lane("das-lane")
        das_target = os.path.join(das, "rust-modules", "target")
        das_fleet = os.path.join(self.fleet, "das-lane", "target")
        local = self.lane("local-lane")
        local_target = os.path.join(local, "rust-modules", "target")
        main_target = os.path.join(self.main, "rust-modules", "target")
        for path in (das_target, das_fleet, local_target, main_target):
            cargo_tree(path, incr=1, hours=40)
        probes = self.foreign(das, os.path.join(self.fleet, "das-lane"))

        i = self.collect(probes)
        by = self.by_path(i)

        for path, kind in ((das_target, "lane"), (das_fleet, "fleet")):
            t = by[path]
            self.assertEqual((t.dev, t.kind, t.owner), (self.OTHER, kind, "das-lane"), path)
            self.assertEqual((t.private_bytes, t.alloc_bytes, t.incr_bytes, t.merged, t.clean), (0, 0, 0, False, False), path)
        self.assertEqual(by[das_target].checkout, das)
        self.assertGreater(by[local_target].private_bytes, 0, "a tree on the build's own volume is still sized")

        build = main_target
        pl = db.plan(i, 1 * GIB, 30 * GIB, TH, self.main, NOW_REAL, build)
        self.assertTrue(pl.actions, "the local trees are still planned")
        touched = {a.tree for a in pl.actions} | {p for a in pl.actions for p in a.paths}
        self.assertFalse([p for p in touched if p in (das_target, das_fleet)], "the planner took a tree on another volume")
        _, kept = db.ladder(i, TH, self.main, NOW_REAL, build)
        for path in (das_target, das_fleet):
            self.assertEqual(kept[path], "never: on another volume than the build", path)
        self.assertEqual(self.foreign_lock_probes, [], "a lock probe reached a tree on another volume")

    def test_a_lane_checkout_on_another_volume_is_never_read_though_its_fleet_tree_is_local(self):
        """A fleet tree is its lane's CARGO_TARGET_DIR kept OUTSIDE the worktree, on the build's own volume,
        while the lane's checkout may sit on the slow disk: the tree is listed and sized like any local one,
        but the lane's git facts (a `git status` over the whole worktree, the squash check) are never asked,
        so such a lane reads neither merged nor clean. The `make check` lock is still asked: it reads a
        local lock file, and a check running in that checkout is building into this very tree."""
        das = self.lane("das-lane")
        fleet_target = os.path.join(self.fleet, "das-lane", "target")
        main_target = os.path.join(self.main, "rust-modules", "target")
        cargo_tree(fleet_target, incr=1, hours=40)
        cargo_tree(main_target, hours=40)
        probes = self.foreign(das, check=(das,))

        t = self.by_path(self.collect(probes))[fleet_target]

        self.assertEqual((t.kind, t.owner, t.checkout), ("fleet", "das-lane", das))
        self.assertNotEqual(t.dev, self.OTHER, "the tree itself is on the build's volume")
        self.assertGreater(t.private_bytes, 0, "a tree on the build's own volume is still sized")
        self.assertEqual((t.merged, t.clean), (False, False))
        self.assertTrue(t.live.check_lock, "a `make check` running in the checkout must still protect the tree")
        self.assertEqual(self.foreign_lock_probes, [das])

    def test_the_report_says_such_a_tree_is_on_another_volume_and_not_sized(self):
        das = self.lane("das-lane")
        das_target = os.path.join(das, "rust-modules", "target")
        cargo_tree(das_target, hours=40)
        cargo_tree(os.path.join(self.main, "rust-modules", "target"), hours=40)
        i = self.collect(self.foreign(das))
        env = dict(self.env, PLX_GC_LOG=os.path.join(self.tmp, "gc.log"))
        text = db.render(i, TH, 5 * GIB, 100 * GIB, self.main, os.path.join(self.main, "rust-modules", "target"),
                         int(3.4 * GIB), NOW_REAL, env)
        row = next(line for line in text.splitlines() if "target" in line and "das-lane" in line)
        self.assertIn("other volume (not sized)", row)


class Report(Repo):
    def run_report(self, free_kib, extra=(), lsof_body="#!/bin/sh\nprintf 'p1\\ncsh\\nfcwd\\nn/\\n'\nexit 1\n", **env):
        bin_dir = tempfile.TemporaryDirectory(prefix="disk-budget-bin-")
        self.addCleanup(bin_dir.cleanup)
        fake = bin_dir.name
        lsof = os.path.join(fake, "lsof")
        with open(lsof, "w") as fh:
            # one minimal valid record (every working lsof lists the caller's own cwd), and macOS's exit 1
            fh.write(lsof_body)
        os.chmod(lsof, 0o755)
        e = clean_env()
        e.update(self.env)
        e.update(PATH=fake + os.pathsep + e.get("PATH", ""), PLX_GC_TEST_FREE_KIB=str(free_kib),
                 PYTHONDONTWRITEBYTECODE="1",
                 PLX_GC_LOG=os.path.join(self.tmp, "gc.log"), HOME=self.tmp)
        e.update(env)
        return subprocess.run([sys.executable, SCRIPT, "report", "--root", self.main, *extra],
                              capture_output=True, text=True, env=e)

    def test_report_prints_the_budget_trees_and_ladder_and_changes_nothing(self):
        cargo_tree(os.path.join(self.tmpr, "ours"), hours=30)
        cargo_tree(os.path.join(self.tmpr, "theirs"), ours=False, hours=30)
        cargo_tree(os.path.join(self.main, "rust-modules", "target"), hours=1)
        before = snapshot_files(self.tmp)
        # A /tmp tree's age is the later of its mtime and its ctime, and a ctime cannot be backdated: the trees
        # above (their mtimes set to 30 hours ago) read as minutes old, so the clock is moved three days on.
        out = self.run_report(8 * 1024 * 1024, PLX_GC_TEST_NOW=str(NOW_REAL + 3 * 86400))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(changed(before, snapshot_files(self.tmp)), [], "report must not create, delete or touch anything")
        text = out.stdout
        for needle in ("disk budget", "comfort line  20.0 GiB", "reserve       3.0 GiB", "would reclaim",
                       "reclaim ladder", "PRIVATE", "not ours", "ours", "origin/main"):
            self.assertIn(needle, text)
        self.assertIn("*", text)

    def test_a_silent_lsof_makes_every_tree_live_in_the_report(self):
        cargo_tree(os.path.join(self.tmpr, "ours"), hours=30)
        out = self.run_report(8 * 1024 * 1024, lsof_body="#!/bin/sh\nexit 1\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("liveness unknown", out.stdout)
        self.assertNotIn("would reclaim", out.stdout)

    def test_report_says_ok_with_plenty_of_room_and_would_refuse_with_none(self):
        self.assertIn("verdict       ok", self.run_report(50 * 1024 * 1024).stdout)
        out = self.run_report(1 * 1024 * 1024, extra=("--need-gib", "30"))
        self.assertIn("WOULD REFUSE", out.stdout)
        self.assertEqual(out.returncode, 0, "a report is information, never a failing exit")


# ---------------------------------------------------------------------------------------------------
# The plan as `build-gc.sh --evict` reads it, and the probes it asks again before every rename.
# ---------------------------------------------------------------------------------------------------
class EmittedPlan(unittest.TestCase):
    def setUp(self):
        self.main = tree("/main/rust-modules/target", kind="main", checkout="/main", owner="main", gib=6, incr=2.5, age=9)
        self.lane = tree("/w/a/rust-modules/target", gib=4, incr=1, age=9)
        self.orphan = tree("/fleet/gone/target", kind="fleet", owner="gone", gib=2, age=9, orphan=True)
        self.tmp_old = tree("/tmp/old", kind="tmp", gib=3, age=40)
        self.mine = tree("/work/me/rust-modules/target-fast", gib=3, incr=1.5, age=9, checkout=ME, owner="me")

    def emitted(self, *trees, free=1 * GIB, need=int(3.4 * GIB), **kw):
        pl = run_plan(inv(*trees), free=free, need=need, **kw)
        lines = db.emit(pl).split("\n")
        return pl, lines, [l.split("\t") for l in lines if l and not l.startswith("#")]

    def test_the_header_names_the_format_and_carries_the_budget(self):
        pl, lines, _ = self.emitted(self.lane)
        self.assertEqual(lines[0], "# plx-disk-plan v1")
        header = db.read_plan_header("\n".join(lines))
        self.assertEqual(header["verdict"], pl.verdict)
        self.assertEqual((int(header["need_kib"]), int(header["reserve_kib"])), (int(3.4 * GIB) // 1024, 3 * GIB // 1024))
        self.assertEqual(int(header["goal_kib"]), pl.goal // 1024)
        self.assertEqual(int(header["free_kib"]), GIB // 1024)

    def test_one_tab_separated_line_per_path_in_ladder_order_with_the_estimate_on_the_first(self):
        _, _, rows = self.emitted(self.orphan, self.tmp_old, self.lane)
        self.assertTrue(all(len(r) == 8 for r in rows), rows)
        self.assertEqual([(r[0], r[1], r[2]) for r in rows],
                         [("evict", "0", "tree"), ("evict", "0", "tree"), ("evict", "2", "incremental"),
                          ("evict", "4", "tree")])
        by_path = {r[7]: r for r in rows}
        self.assertEqual(by_path["/fleet/gone/target"][3], str(2 * GIB // 1024))
        self.assertEqual(by_path["/fleet/gone/target"][4], "gone")
        self.assertEqual(by_path["/w/a/rust-modules/target"][6], "normal")

    def test_the_age_gate_each_action_needs_the_executor_to_find_again_is_in_minutes(self):
        _, _, rows = self.emitted(self.orphan, self.tmp_old, self.lane)
        idle = {r[7]: int(r[5]) for r in rows}
        self.assertEqual(idle["/fleet/gone/target"], 0)              # an orphan: nothing refers to it
        self.assertEqual(idle["/tmp/old"], 24 * 60)                  # /tmp garbage: a day
        self.assertEqual(idle["/w/a/rust-modules/target"], 15)       # an idle lane's tree: the idle window

    def test_the_stale_sweep_is_one_line_at_its_rung_5_slot(self):
        _, _, rows = self.emitted(self.main, self.lane, need=20 * GIB)
        stale = [r for r in rows if r[0] == "stale"]
        self.assertEqual(len(stale), 1)
        self.assertEqual(stale[0][1], "5")
        rungs = [int(r[1]) for r in rows]
        self.assertEqual(rungs, sorted(rungs), "the plan must be in rung order")

    def test_rung_6_in_the_current_checkout_is_marked_current_and_the_main_one_is_not(self):
        _, _, rows = self.emitted(self.main, self.mine, free=0, need=20 * GIB)
        modes = {r[7]: r[6] for r in rows if r[1] == "6"}
        self.assertEqual(modes["/work/me/rust-modules/target-fast/debug/incremental"], "current")
        self.assertEqual(modes["/main/rust-modules/target/debug/incremental"], "normal")

    def test_a_path_with_a_control_character_is_left_out_not_split(self):
        odd = tree("/w/odd\tname/rust-modules/target", gib=4, age=9, checkout="/w/odd\tname")
        _, lines, rows = self.emitted(odd)
        self.assertEqual(rows, [])
        self.assertTrue(any(l.startswith("# skipped") for l in lines))


class Seams(unittest.TestCase):
    def test_the_clock_moves_only_when_the_tmp_roots_are_named(self):
        self.assertAlmostEqual(db.clock({}), __import__("time").time(), delta=5)
        self.assertAlmostEqual(db.clock({"PLX_GC_TEST_NOW": "5"}), __import__("time").time(), delta=5,
                               msg="a pretend clock over the default roots would age the owner's real trees")
        self.assertEqual(db.clock({"PLX_GC_TEST_NOW": "5", "PLX_GC_TMP_ROOTS": "/x"}), 5.0)
        self.assertAlmostEqual(db.clock({"PLX_GC_TEST_NOW": "junk", "PLX_GC_TMP_ROOTS": "/x"}),
                               __import__("time").time(), delta=5)

    def test_a_pretend_emergency_over_a_default_root_is_refused(self):
        roots = {"PLX_GC_TMP_ROOTS": "/x", "PLX_FLEET_DIR": "/y", "PLX_BUILD_CACHE": "/z"}
        self.assertEqual(db.refuse_pretend_emergency(roots), "")
        self.assertEqual(db.refuse_pretend_emergency({}), "", "no override: nothing to refuse")
        for override in ({"PLX_GC_TEST_FREE_KIB": "1"}, {"PLX_GC_TEST_NOW": "9"}):
            self.assertEqual(db.refuse_pretend_emergency({**roots, **override}), "")
            for missing in roots:
                env = {**roots, **override}
                del env[missing]
                self.assertIn(missing, db.refuse_pretend_emergency(env), (override, missing))
        # an empty PLX_FLEET_DIR is a choice ("no fleet"), not a default
        self.assertEqual(db.refuse_pretend_emergency({**roots, "PLX_FLEET_DIR": "", "PLX_GC_TEST_FREE_KIB": "1"}), "")


class Subcommands(Repo):
    """`reclaim --emit`, `tmp-trees`, `probe` and `settle` as processes, against real directories."""

    def run_tool(self, *args, env=None, lsof_body="#!/bin/sh\nprintf 'p1\\ncsh\\nfcwd\\nn/\\n'\nexit 1\n", plain=False):
        bin_dir = tempfile.TemporaryDirectory(prefix="disk-budget-bin-")
        self.addCleanup(bin_dir.cleanup)
        lsof = os.path.join(bin_dir.name, "lsof")
        with open(lsof, "w") as fh:
            fh.write(lsof_body)
        os.chmod(lsof, 0o755)
        e = clean_env()
        e.update(self.env)
        e.update(PATH=bin_dir.name + os.pathsep + e.get("PATH", ""), PYTHONDONTWRITEBYTECODE="1", HOME=self.tmp)
        e.update(env or {})
        return subprocess.run([sys.executable, SCRIPT, *args], capture_output=True, text=True, env=e)

    def ours(self, name="ours", hours=30):
        path = os.path.join(self.tmpr, name)
        cargo_tree(path, hours=hours)
        return path

    # -- reclaim --emit ----------------------------------------------------------------------------
    def test_reclaim_emit_prints_a_plan_build_gc_can_read(self):
        old = self.ours()
        cargo_tree(os.path.join(self.fleet, "gone", "target"), hours=6)
        out = self.run_tool("reclaim", "--to", "20", "--no-build", "--root", self.main, "--emit",
                            env={"PLX_GC_TEST_FREE_KIB": str(10 * 1024 * 1024), "PLX_GC_TEST_NOW": str(NOW_REAL + 3 * 86400)})
        self.assertEqual(out.returncode, 0, out.stderr)
        lines = out.stdout.split("\n")
        self.assertEqual(lines[0], "# plx-disk-plan v1")
        rows = [l.split("\t") for l in lines if l.startswith(("evict", "stale"))]
        paths = {r[7] for r in rows}
        self.assertIn(db.real(old), paths)
        self.assertIn(db.real(os.path.join(self.fleet, "gone", "target")), paths)
        self.assertEqual(db.read_plan_header(out.stdout)["goal_kib"], str(24 * 1024 * 1024))

    def test_reclaim_refuses_to_plan_a_pretend_emergency_over_the_real_roots(self):
        for override in ({"PLX_GC_TEST_FREE_KIB": "1"}, {"PLX_GC_TEST_NOW": "9999999999"}):
            for missing in ("PLX_GC_TMP_ROOTS", "PLX_FLEET_DIR", "PLX_BUILD_CACHE"):
                with self.subTest(override=list(override), missing=missing):
                    e = dict(self.env)
                    e.update(override)
                    out = subprocess.run([sys.executable, SCRIPT, "reclaim", "--to", "20", "--no-build",
                                          "--root", self.main, "--emit"], capture_output=True, text=True,
                                         env={**clean_env(), **{k: v for k, v in e.items() if k != missing},
                                              "HOME": self.tmp, "PYTHONDONTWRITEBYTECODE": "1"})
                    self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
                    self.assertEqual(out.stdout, "", "a refused plan must print nothing a caller could run")
                    self.assertIn(missing, out.stderr)

    def test_a_tmp_tree_with_a_backdated_mtime_is_never_a_candidate(self):
        # `utime` backdates the mtime; the ctime is the kernel's. The tree below was written minutes ago.
        path = self.ours(hours=30 * 24)
        i = self.collect()
        t = self.by_path(i)[db.real(path)]
        self.assertGreater(t.last_built, NOW_REAL - 300, "the tree reads as old: its ctime was not consulted")
        actions, kept = db.ladder(i, TH, None, NOW_REAL)
        self.assertNotIn(t.path, {a.tree for a in actions})
        self.assertIn("built", kept[t.path])
        # the same tree with the clock three days on is old garbage
        later, _ = db.ladder(i, TH, None, NOW_REAL + 3 * 86400)
        self.assertIn((0, t.path), {(a.rung, a.tree) for a in later})

    def test_newest_mtime_with_ctime_is_the_later_of_the_two(self):
        path = self.ours(hours=48)
        self.assertLess(db.newest_mtime(path), NOW_REAL - 40 * 3600)
        self.assertGreater(db.newest_mtime(path, ctime=True), NOW_REAL - 300)

    # -- tmp-trees ----------------------------------------------------------------------------------
    def test_tmp_trees_lists_ours_and_skips_worktrees_the_fleet_dir_and_foreign_trees(self):
        ours = self.ours()
        cargo_tree(os.path.join(self.tmpr, "theirs"), ours=False)
        lane = self.lane("lane-a")
        cargo_tree(os.path.join(lane, "rust-modules", "target"))
        # a tmp root that contains a worktree and the fleet dir: neither is a /tmp tree
        inside = os.path.join(self.tmpr, "wt")
        git(self.main, "worktree", "add", "-q", "-b", "wt", inside)
        cargo_tree(os.path.join(inside, "target"))
        env = {"PLX_GC_TMP_ROOTS": os.pathsep.join([self.tmpr, self.fleet])}
        cargo_tree(os.path.join(self.fleet, "gone", "target"))
        out = self.run_tool("tmp-trees", "--root", self.main, env=env)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.split(), [db.real(ours)])

    # -- probe --------------------------------------------------------------------------------------
    def probe(self, path, klass="lane", *extra, env=None, **kw):
        return self.run_tool("probe", "--root", self.main, "--tree", path, "--class", klass, *extra, env=env, **kw)

    def test_probe_says_idle_for_a_quiet_lane_tree(self):
        lane = self.lane("lane-a")
        path = os.path.join(lane, "rust-modules", "target")
        cargo_tree(path)
        out = self.probe(path, "lane", "--checkout", lane)
        self.assertEqual((out.returncode, out.stdout.strip()), (0, "idle"), out.stderr)

    def test_probe_refuses_a_tree_that_holds_a_tracked_file(self):
        lane = self.lane("lane-a")
        path = os.path.join(lane, "rust-modules", "target-kept")
        cargo_tree(path)
        git(lane, "add", "-f", os.path.join("rust-modules", "target-kept", "CACHEDIR.TAG"))
        git(lane, "commit", "-q", "-m", "tracked")
        out = self.probe(path, "lane", "--checkout", lane)
        self.assertEqual(out.returncode, 1, out.stdout)
        self.assertIn("git tracks", out.stdout)

    def test_probe_sees_a_held_cargo_lock_and_an_open_file(self):
        import fcntl
        lane = self.lane("lane-a")
        path = os.path.join(lane, "rust-modules", "target")
        cargo_tree(path)
        lock = os.path.join(path, "debug", ".cargo-lock")
        write(lock, 0)
        fd = os.open(lock, os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            out = self.probe(path, "lane", "--checkout", lane)
        finally:
            os.close(fd)
        self.assertEqual(out.returncode, 1, out.stdout)
        self.assertIn("cargo lock", out.stdout)
        opened = self.probe(path, "lane", "--checkout", lane,
                            lsof_body="#!/bin/sh\nprintf 'p7\\ncrustc\\nf9\\nn%s\\n'\nexit 0\n" % os.path.join(path, "debug", "deps", "x.o"))
        self.assertEqual(opened.returncode, 1, opened.stdout)

    def test_probe_treats_an_lsof_that_says_nothing_as_live(self):
        lane = self.lane("lane-a")
        path = os.path.join(lane, "rust-modules", "target")
        cargo_tree(path)
        out = self.probe(path, "lane", "--checkout", lane, lsof_body="#!/bin/sh\nexit 1\n")
        self.assertEqual(out.returncode, 1, out.stdout)
        self.assertIn("unknown", out.stdout)

    def test_probe_applies_the_current_checkouts_rule_only_when_told(self):
        lane = self.lane("lane-a")
        path = os.path.join(lane, "rust-modules", "target-fast")
        cargo_tree(path)
        cwd_in_lane = "#!/bin/sh\nprintf 'p424242\\ncmake\\nfcwd\\nn%s\\n'\nexit 0\n" % lane
        other = self.probe(path, "lane", "--checkout", lane, lsof_body=cwd_in_lane)
        self.assertEqual(other.returncode, 1, "a builder in another lane's checkout must protect its tree")
        current = self.probe(path, "lane", "--checkout", lane, "--current", lsof_body=cwd_in_lane)
        self.assertEqual(current.returncode, 0, "`make` itself runs in the current checkout: " + current.stdout)

    def test_probe_holds_a_tmp_tree_to_its_ctime_not_its_mtime(self):
        path = self.ours(hours=30 * 24)
        young = self.probe(path, "tmp")
        self.assertEqual(young.returncode, 1, "a fixture with a backdated mtime is not old: " + young.stdout)
        self.assertIn("changed", young.stdout)
        older = self.probe(path, "tmp", env={"PLX_GC_TEST_NOW": str(NOW_REAL + 3 * 86400)})
        self.assertEqual((older.returncode, older.stdout.strip()), (0, "idle"), older.stderr)
        # and the age the plan asks for cannot be talked below the idle window
        below = self.probe(path, "tmp", "--min-age-secs", "0", env={"PLX_GC_TEST_NOW": str(NOW_REAL + 60)})
        self.assertEqual(below.returncode, 1, below.stdout)

    def test_probe_leaves_a_young_seed_leftover_and_takes_an_old_one_or_a_tombstone(self):
        seeds = os.path.join(self.cache, "cargo-seed")
        fresh = os.path.join(seeds, "target.tmp.77")
        stale = os.path.join(seeds, "target.old.77")
        tomb = os.path.join(seeds, "target.tombstone.77.1")
        for d in (fresh, stale, tomb):
            write(os.path.join(d, "x"), 16)
        age_all(stale, 3 * 24)
        self.assertEqual(self.probe(fresh, "leftover").returncode, 1)
        self.assertEqual(self.probe(stale, "leftover").returncode, 0)
        self.assertEqual(self.probe(tomb, "leftover").returncode, 0, "an interrupted delete is never in flight")

    # -- settle -------------------------------------------------------------------------------------
    def test_settle_exits_0_when_the_free_space_covers_the_plans_budget_and_3_when_it_does_not(self):
        plan = os.path.join(self.tmp, "plan.tsv")
        with open(plan, "w") as fh:
            fh.write("# plx-disk-plan v1\n# need_kib %d\n# reserve_kib %d\n" % (4 << 20, 3 << 20))
        ok = self.run_tool("settle", "--plan", plan, "--free-kib", str(8 << 20))
        self.assertEqual((ok.returncode, ok.stdout.split(":")[0]), (0, "ok"), ok.stderr)
        short = self.run_tool("settle", "--plan", plan, "--free-kib", str(6 << 20))
        self.assertEqual((short.returncode, short.stdout.split(":")[0]), (3, "REFUSE"), short.stderr)
        with open(plan, "w") as fh:
            fh.write("# plx-disk-plan v1\n")
        self.assertEqual(self.run_tool("settle", "--plan", plan, "--free-kib", "1").returncode, 0)


class IndexIsNeverRefreshed(Repo):
    def test_collecting_the_inventory_does_not_rewrite_a_lanes_index(self):
        # `git status` refreshes (rewrites) the index when a tracked file's stat data went stale unless
        # GIT_OPTIONAL_LOCKS=0: a read-only report that did so would race the lane's own `git add`. The control
        # shows this fixture CAN see the write. The lane is squash-landed because only a MERGED lane is asked
        # whether it is clean (`git status`), the one call here that can refresh an index.
        lane = self.lane("lane-a", {"a.txt": "a"})
        self.squash_into_origin(lane)
        cargo_tree(os.path.join(lane, "rust-modules", "target"))
        index = git(lane, "rev-parse", "--path-format=absolute", "--git-path", "index")
        later = NOW_REAL + 120
        os.utime(os.path.join(lane, "a.txt"), (later, later))
        def state():
            with open(index, "rb") as fh:
                return fh.read(), os.stat(index).st_mtime_ns
        before = state()
        subprocess.run(["git", "-C", lane, "status", "--porcelain"], env=clean_env(), check=True, capture_output=True)
        self.assertNotEqual(before, state(), "the control `git status` did not refresh the index: this test cannot fail")
        os.utime(os.path.join(lane, "a.txt"), (later + 60, later + 60))
        before = state()
        self.collect()
        self.assertEqual(before, state(), "collect() refreshed a lane's index")


# ---------------------------------------------------------------------------------------------------
# The cold store's park-ahead pass: which idle trees are candidates (a pure function of the inventory),
# the probe that is asked again right before a tree moves, the pass that parks them up to a budget, and the
# waiting-preflight flag that stops it between trees. docs/disk-budget.md, "The cold store".
# ---------------------------------------------------------------------------------------------------
COLD_SCRIPT = os.path.join(ROOT, "tools", "cold-store.py")
cold_spec = importlib.util.spec_from_file_location("cold_store_for_disk_budget", COLD_SCRIPT)
cold = importlib.util.module_from_spec(cold_spec)
cold_spec.loader.exec_module(cold)
PARK_TH = db.Thresholds(idle_secs=2 * HOUR)


def copy_stub(src, dst):
    shutil.copytree(src, dst, symlinks=True)


def candidate_paths(i, th=PARK_TH, now=NOW):
    return [c.path for c in db.park_candidates(i, th, now)]


class ParkCandidates(unittest.TestCase):
    def setUp(self):
        self.i = inv(
            tree("/w/a/rust-modules/target", gib=4, age=3),                          # idle lane, 3 h
            tree("/w/b/rust-modules/target", gib=4, age=9),                          # idle lane, 9 h (oldest lane)
            tree("/w/y/rust-modules/target", gib=4, age=1),                          # young lane, 1 h: under the 2 h idle
            tree("/w/c/rust-modules/target", gib=3, incr=1, age=6),                  # lane with an incremental dir
            tree("/tmp/young", kind="tmp", gib=2, age=4),                            # /tmp tree, idle 4 h (rung 3)
            tree("/tmp/old", kind="tmp", gib=2, age=50),                             # /tmp garbage: deleted, not parked
            tree("/fleet/gone/target", kind="fleet", gib=1.5, age=30, orphan=True),  # orphan: garbage
            tree("/w/m/rust-modules/target", gib=2, age=12, checkout="/w/m", owner="m", merged=True, clean=True),
            tree("/w/md/rust-modules/target", gib=2, age=13, checkout="/w/md", owner="md", merged=True),
            tree("/w/live/rust-modules/target", gib=3, age=8, live=live(open_files=("rustc",))),
            tree("/main/rust-modules/target", kind="main", gib=5, age=20, checkout="/main", owner="main"),
            tree("/cache/cargo-seed/abc", kind="seed", gib=0.5, age=90, checkout=None),
            tree("/mnt/slow/rust-modules/target", gib=9, age=40, dev=99),            # another volume
            tree("/w/t/rust-modules/target", gib=2, age=7, tracked=True),
            tree("/w/z/rust-modules/target", gib=0, age=7),
            volume_dev=1)

    def test_only_idle_whole_lane_trees_and_idle_tmp_trees_are_candidates_oldest_first(self):
        got = candidate_paths(self.i)
        self.assertEqual(got, ["/w/b/rust-modules/target", "/w/c/rust-modules/target", "/tmp/young",
                               "/w/a/rust-modules/target"])
        by = {c.path: c for c in db.park_candidates(self.i, PARK_TH, NOW)}
        self.assertEqual(by["/w/b/rust-modules/target"].kind, "lane")
        self.assertEqual(by["/w/b/rust-modules/target"].checkout, "/w/b")
        self.assertEqual(by["/tmp/young"].kind, "tmp")
        self.assertEqual(by["/tmp/young"].checkout, "")
        self.assertEqual(by["/w/c/rust-modules/target"].private_bytes, 3 * GIB,
                         "a parked tree frees its WHOLE private size, the incremental dir included")

    def test_never_garbage_main_seed_live_young_merged_foreign_tracked_or_empty_trees(self):
        got = set(candidate_paths(self.i))
        for kept in ("/tmp/old", "/fleet/gone/target", "/w/m/rust-modules/target", "/w/md/rust-modules/target",
                     "/w/live/rust-modules/target", "/main/rust-modules/target", "/cache/cargo-seed/abc",
                     "/mnt/slow/rust-modules/target", "/w/t/rust-modules/target", "/w/z/rust-modules/target",
                     "/w/y/rust-modules/target"):
            self.assertNotIn(kept, got)

    def test_the_idle_window_is_the_one_the_caller_passes(self):
        short = candidate_paths(self.i, th=db.Thresholds(idle_secs=30 * 60))
        self.assertIn("/w/y/rust-modules/target", short)
        self.assertNotIn("/w/y/rust-modules/target", candidate_paths(self.i, th=db.Thresholds(idle_secs=2 * HOUR)))

    def test_the_candidate_list_is_the_ladders_own_no_build_view(self):
        actions, _ = db.ladder(self.i, PARK_TH, None, NOW)
        whole = {a.tree for a in actions if a.rung in (3, 4) and a.scope == "tree"}
        self.assertLessEqual(set(candidate_paths(self.i)), whole)

    def test_emit_is_one_tab_separated_line_per_candidate(self):
        text = db.emit_candidates(db.park_candidates(self.i, PARK_TH, NOW))
        lines = text.split("\n")
        self.assertEqual(lines[0], "# plx-park-candidates v1")
        rows = [l.split("\t") for l in lines[1:]]
        self.assertEqual([r[0] for r in rows], ["park"] * 4)
        self.assertEqual(rows[0], ["park", "lane", str(4 * GIB // 1024), "b", "/w/b", "/w/b/rust-modules/target"])
        self.assertEqual(rows[2][3:5], ["-", "-"], "a /tmp tree has no owner and no checkout")


class LaneIdle(Repo):
    def aged_lane(self, name):
        lane = self.lane(name)
        target = os.path.join(lane, "rust-modules", "target")
        cargo_tree(target, hours=5)
        for dirpath, dirs, files in os.walk(lane):
            dirs[:] = [d for d in dirs if d != ".git"]
            for n in files + dirs:
                os.utime(os.path.join(dirpath, n), (NOW_REAL - 9 * HOUR,) * 2, follow_symlinks=False)
        os.utime(lane, (NOW_REAL - 9 * HOUR,) * 2)
        return lane, target

    def ask(self, lane, target, secs):
        return db.probe_tree(target, lane, None, "lane", False, 0, QuietProbes(), [lane, self.main], NOW_REAL,
                             lane_idle_secs=secs)

    def test_a_lane_whose_source_changed_recently_is_not_idle(self):
        lane, target = self.aged_lane("busy")
        self.assertEqual(self.ask(lane, target, 2 * HOUR), [])
        self.assertEqual(self.ask(lane, target, 0), [], "lane_idle_secs=0 asks no lane question at all")
        write(os.path.join(lane, "src", "edited.rs"), 8)                     # a source edit, just now
        why = self.ask(lane, target, 2 * HOUR)
        self.assertTrue(any("changed" in r and "lane" in r for r in why), why)
        self.assertEqual(self.ask(lane, target, 0), [])

    def test_a_change_inside_dot_git_does_not_count(self):
        plain = os.path.join(self.tmp, "plain")                    # a checkout with a real `.git` directory
        target = os.path.join(plain, "rust-modules", "target")
        cargo_tree(target, hours=5)
        write(os.path.join(plain, "src", "a.rs"), 8)
        for dirpath, dirs, files in os.walk(plain):
            for n in files + dirs:
                os.utime(os.path.join(dirpath, n), (NOW_REAL - 9 * HOUR,) * 2, follow_symlinks=False)
        os.utime(plain, (NOW_REAL - 9 * HOUR,) * 2)
        write(os.path.join(plain, ".git", "objects", "ab", "new"), 8)        # git's own housekeeping, just now
        os.utime(plain, (NOW_REAL - 9 * HOUR,) * 2)                          # (creating `.git` touched its parent)
        self.assertEqual(self.ask(plain, target, 2 * HOUR), [])
        write(os.path.join(plain, "src", "b.rs"), 8)
        self.assertNotEqual(self.ask(plain, target, 2 * HOUR), [])

    def test_a_lane_checkout_on_another_volume_is_not_walked_and_counts_as_busy(self):
        lane, target = self.aged_lane("slow")
        real_dev = db.device_of
        with mock.patch.object(db, "device_of", lambda p: 7 if p == lane else real_dev(p)):
            why = self.ask(lane, target, 2 * HOUR)
        self.assertTrue(any("another volume" in r for r in why), why)


class CountingLock:
    """The auto lock as the pass sees it: cycles are counted, and a test says when the re-take fails."""

    def __init__(self, retake=True):
        self.cycles, self.released, self.retake = 0, False, retake

    def cycle(self):
        self.cycles += 1
        return self.retake

    def release(self):
        self.released = True


class ParkPass(Repo):
    """db.run_park_pass against a real inventory, a real cold store (the two volumes faked) and stub probes."""

    def setUp(self):
        super().setUp()
        self.cold_root = os.path.join(self.tmp, "das", "cold")
        os.makedirs(self.cold_root)
        cfg = os.path.join(self.tmp, "cold-store.cfg")
        with open(cfg, "w") as fh:
            fh.write("root=%s\nmin_free_gib=0\npark_idle_min=120\n" % self.cold_root)
        self.env.update(PLX_COLD_STORE_CONFIG=cfg, PLX_TV_CONFIG_DIR=os.path.join(self.tmp, "none"))
        das = os.path.join(self.tmp, "das")
        real_dev = cold.device_of
        for name, fake in (("device_of", lambda p: 2 if cold.norm(p).startswith(das) else real_dev(p)),
                           ("which_ditto", lambda: "/usr/bin/ditto"), ("copy_tree", copy_stub),
                           ("flush_cold_volume", lambda path: None)):
            patcher = mock.patch.object(cold, name, fake)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.store = cold.ColdStore(self.env, log=lambda m: None)
        self.lines = []
        self.lanes = [self.idle_lane("p%d" % n, hours=9 - n) for n in range(3)]

    def idle_lane(self, name, hours, trees=("target",)):
        lane = self.lane(name, {name + ".txt": "x"})
        for tree_name in trees:
            cargo_tree(os.path.join(lane, "rust-modules", tree_name), size=256 * 1024, hours=hours)
        for dirpath, dirs, files in os.walk(lane):
            dirs[:] = [d for d in dirs if d != ".git"]
            for n in files + dirs:
                os.utime(os.path.join(dirpath, n), (NOW_REAL - hours * HOUR,) * 2, follow_symlinks=False)
        os.utime(lane, (NOW_REAL - hours * HOUR,) * 2)
        return lane

    def candidates(self, probes=None):
        return db.park_candidates(self.collect(probes), PARK_TH, NOW_REAL)

    def run_pass(self, probes=None, max_bytes=1 << 40, lock=None, waiting=None, dry_run=False, free=1 * GIB,
                 stop_free=50 * GIB, cands=None, floor_free=None, store=None):
        """`free` is a byte count, or a callable that answers each look at the free space (one per tree)."""
        i = self.collect(probes)
        cands = cands if cands is not None else db.park_candidates(i, PARK_TH, NOW_REAL)
        return db.run_park_pass(store or self.store, cands, probes=probes or QuietProbes(),
                                checkouts=[w for w, _ in i.checkouts], now=lambda: NOW_REAL, idle_secs=2 * HOUR,
                                max_bytes=max_bytes, stop_free=stop_free, floor_free=floor_free,
                                free_fn=free if callable(free) else (lambda: free),
                                head_of=lambda c: "head", lock=lock, waiting=waiting or (lambda: False),
                                say=self.lines.append, dry_run=dry_run)

    def target(self, lane):
        return os.path.join(lane, "rust-modules", "target")

    def test_idle_trees_park_oldest_first_and_leave_the_lane_without_them(self):
        done = self.run_pass()
        self.assertEqual([o.status for _, o in done.outcomes], [cold.PARKED] * 3, self.lines)
        self.assertEqual([c.owner for c, _ in done.outcomes], ["p0", "p1", "p2"])
        for lane in self.lanes:
            self.assertFalse(os.path.exists(self.target(lane)))
        self.assertEqual(len(self.store.records()), 3)
        self.assertEqual(done.stopped, "")
        self.assertGreater(done.parked_bytes, 0)
        self.assertEqual(len([l for l in self.lines if l.startswith("  parked")]), 3, self.lines)

    def test_a_tree_that_turned_live_since_the_inventory_stays_and_the_others_go(self):
        cands = self.candidates()
        probes = QuietProbes(cargo={db.real(self.target(self.lanes[1]))})
        done = self.run_pass(probes=probes, cands=cands)
        self.assertEqual({c.owner: o.status for c, o in done.outcomes},
                         {"p0": cold.PARKED, "p1": cold.REFUSED, "p2": cold.PARKED})
        self.assertTrue(os.path.isdir(self.target(self.lanes[1])))
        self.assertTrue(any("kept" in l and "p1" in l for l in self.lines), self.lines)

    def test_a_lane_edited_since_the_inventory_is_not_parked(self):
        cands = self.candidates()
        write(os.path.join(self.lanes[0], "src", "new.rs"), 8)
        done = self.run_pass(cands=cands)
        self.assertEqual({c.owner: o.status for c, o in done.outcomes}["p0"], cold.REFUSED)
        self.assertTrue(os.path.isdir(self.target(self.lanes[0])))

    def test_a_second_tree_of_a_lane_this_pass_took_from_is_not_refused_for_the_pass_own_rename(self):
        # Moving a lane's first tree out changes its `rust-modules/` mtime: the lane must not then read as busy
        # to the next tree of the same lane (build-gc's `--evict` skips a lane it has already taken from too).
        two = self.idle_lane("two", hours=5, trees=("target", "target-fast"))
        done = self.run_pass()
        self.assertEqual([o.status for _, o in done.outcomes], [cold.PARKED] * 5, self.lines)
        for name in ("target", "target-fast"):
            self.assertFalse(os.path.exists(os.path.join(two, "rust-modules", name)))

    def test_the_budget_stops_the_pass_after_the_tree_that_reaches_it(self):
        one = self.candidates()[0].private_bytes
        done = self.run_pass(max_bytes=one)
        self.assertEqual(len(done.outcomes), 1)
        self.assertIn("park_max_gib_per_run", done.stopped)
        self.assertTrue(os.path.isdir(self.target(self.lanes[1])))

    def test_the_pass_stops_when_free_space_is_back_over_the_line(self):
        done = self.run_pass(free=60 * GIB, stop_free=50 * GIB)
        self.assertEqual(done.outcomes, [])
        self.assertIn("free space", done.stopped)

    def test_nothing_is_parked_while_free_space_is_under_the_comfort_line(self):
        # Parking holds the lock for a whole copy of a slow disk; a build waiting for room must get the ladder's
        # deletions, which take seconds, so below the comfort line the pass does not start a tree.
        done = self.run_pass(free=10 * GIB, floor_free=20 * GIB)
        self.assertEqual(done.outcomes, [])
        self.assertIn("comfort line", done.stopped)
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes))
        self.assertEqual(self.store.records(), [])

    def test_exactly_the_comfort_line_still_parks(self):
        done = self.run_pass(free=20 * GIB, floor_free=20 * GIB)
        self.assertEqual([o.status for _, o in done.outcomes], [cold.PARKED] * 3, self.lines)

    def test_free_space_falling_under_the_comfort_line_between_trees_ends_the_pass(self):
        levels = iter([30 * GIB, 19 * GIB])               # a look before each tree; builds ate the room meanwhile
        done = self.run_pass(free=lambda: next(levels), floor_free=20 * GIB)
        self.assertEqual(len(done.outcomes), 1, "the tree in hand finishes, the next one is not started")
        self.assertIn("comfort line", done.stopped)
        self.assertIn("park: stopped", " ".join(self.lines))
        self.assertTrue(os.path.isdir(self.target(self.lanes[1])))

    def test_a_by_hand_pass_has_no_comfort_line_to_keep(self):
        done = self.run_pass(free=1 * GIB, floor_free=None, stop_free=None)
        self.assertEqual([o.status for _, o in done.outcomes], [cold.PARKED] * 3, self.lines)

    def test_an_exception_out_of_a_park_is_a_logged_failure_that_ends_the_pass(self):
        with mock.patch.object(self.store, "park", side_effect=ValueError("surprise")):
            done = self.run_pass()
        self.assertEqual([o.status for _, o in done.outcomes], [db._FAILED])
        self.assertIn("surprise", done.stopped)
        self.assertTrue(any(l.startswith("park: stopped:") and "surprise" in l for l in self.lines), self.lines)
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes))

    def test_an_exception_out_of_the_volume_check_ends_the_pass_without_a_traceback(self):
        with mock.patch.object(self.store, "available", side_effect=OSError("device not configured")):
            done = self.run_pass()
        self.assertEqual(done.outcomes, [])
        self.assertIn("device not configured", done.stopped)

    def test_the_lock_is_released_and_re_taken_between_trees_not_after_the_last(self):
        lock = CountingLock()
        done = self.run_pass(lock=lock)
        self.assertEqual(lock.cycles, 2, "three trees: a cycle between each pair")
        self.assertFalse(lock.released)
        self.assertFalse(done.lock_lost)

    def test_a_waiting_preflight_stops_the_pass_after_the_current_tree_and_hands_the_lock_over(self):
        calls = []

        def waiting():
            calls.append(1)
            return len(calls) > 1                  # nobody before the first tree, a waiter from then on

        lock = CountingLock()
        done = self.run_pass(lock=lock, waiting=waiting)
        self.assertEqual(len(done.outcomes), 1, "the current tree finishes, the next is not started")
        self.assertTrue(lock.released and done.lock_lost)
        self.assertEqual(lock.cycles, 0)
        self.assertIn("waiting", done.stopped)
        self.assertTrue(os.path.isdir(self.target(self.lanes[1])))

    def test_a_preflight_already_waiting_means_no_tree_is_started(self):
        lock = CountingLock()
        done = self.run_pass(lock=lock, waiting=lambda: True)
        self.assertEqual(done.outcomes, [])
        self.assertTrue(lock.released and done.lock_lost)
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes))

    def test_a_lock_that_cannot_be_re_taken_ends_the_pass(self):
        lock = CountingLock(retake=False)
        done = self.run_pass(lock=lock)
        self.assertEqual(len(done.outcomes), 1)
        self.assertTrue(done.lock_lost)
        self.assertIn("lock", done.stopped)

    def test_a_dry_run_names_what_would_move_and_moves_nothing(self):
        def shape():
            return sorted((p, os.path.getsize(p) if os.path.isfile(p) else -1)
                          for d, ds, fs in os.walk(self.tmp) if ".git" not in d.split(os.sep)
                          for p in [os.path.join(d, n) for n in ds + fs])
        before = shape()
        done = self.run_pass(dry_run=True)
        self.assertEqual([o.status for _, o in done.outcomes], [cold.DRY_RUN] * 3)
        self.assertEqual(before, shape(), "a dry run created or removed something (the cold root, a probe file, a "
                                          "parking directory)")
        self.assertEqual(len([l for l in self.lines if l.startswith("  would park")]), 3, self.lines)

    def test_a_failed_copy_stops_the_pass_and_the_tree_is_back(self):
        def boom(src, dst):
            raise OSError("cold disk went away")
        with mock.patch.object(cold, "copy_tree", boom):
            done = self.run_pass()
        self.assertEqual([o.status for _, o in done.outcomes], [cold.FAILED])
        self.assertIn("stopped", " ".join(self.lines))
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes))

    def test_a_volume_that_is_not_usable_is_said_once_and_nothing_is_tried(self):
        with mock.patch.object(cold, "which_ditto", lambda: None):
            done = self.run_pass()
        self.assertEqual(done.outcomes, [])
        self.assertIn("ditto", done.stopped)
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes))

    # --- park_run: the sweeps around the pass look at the waiting flag too -------------------------------------
    def lock_and_env(self):
        env = dict(self.env, PLX_GC_LOCK_DIR=os.path.join(self.tmp, "gc.lock"))
        path = db.lock_dir(env)
        self.assertEqual(db.take_lock(path, env, os.getpid()), "got")
        return env, db.HeldLock(path, env, os.getpid())

    def call_park_run(self, env, lock, auto=True):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = db.park_run(argparse.Namespace(dry_run=False, auto=auto), env, self.main, cold, self.store, 1 * GIB,
                             2 * HOUR, lock)
        return rc, out.getvalue()

    def test_a_preflight_already_waiting_stops_recover_before_it_deletes_anything_and_gets_the_lock(self):
        env, lock = self.lock_and_env()
        debris = os.path.join(self.cold_root, "trees", "x.consumed")
        os.makedirs(debris)
        db.announce_waiting(lock.path)
        self.addCleanup(db.withdraw_waiting, lock.path)
        rc, out = self.call_park_run(env, lock)
        self.assertEqual(rc, db.PARK_LOCK_RELEASED, out)
        self.assertFalse(lock.held or os.path.exists(lock.path), "the lock must be free for the waiter")
        self.assertTrue(os.path.isdir(debris), "recover reaped cold-side debris with a preflight waiting")
        self.assertEqual(self.store.records(), [])
        self.assertTrue(all(os.path.isdir(self.target(l)) for l in self.lanes), "a tree was parked")
        self.assertIn("waiting", out)

    def test_a_preflight_that_starts_waiting_during_the_pass_gets_the_lock_before_the_prune(self):
        self.run_pass()                                         # three parked trees, each record naming its lane
        env, lock = self.lock_and_env()
        asked = {}

        def fake_pass(store, cands, **kw):
            asked.update(kw)
            db.announce_waiting(lock.path)
            return db.ParkSummary([])
        self.addCleanup(db.withdraw_waiting, lock.path)
        with mock.patch.object(db, "run_park_pass", fake_pass), \
                mock.patch.object(db, "prune_facts", lambda *a: cold.PruneFacts(gone={self.lanes[0]})):
            rc, out = self.call_park_run(env, lock)
        self.assertEqual(rc, db.PARK_LOCK_RELEASED, out)
        self.assertFalse(lock.held or os.path.exists(lock.path), "the lock must be free for the waiter")
        self.assertEqual(len(self.store.records()), 3, "the prune dropped a copy with a preflight waiting: " + out)
        self.assertEqual(asked["floor_free"], db.DEFAULT_COMFORT_GIB * GIB, "--auto keeps the comfort line")

    def test_without_a_waiter_the_prune_runs_and_the_lock_stays_with_the_caller(self):
        self.run_pass()
        env, lock = self.lock_and_env()
        asked = {}

        def fake_pass(store, cands, **kw):
            asked.update(kw)
            return db.ParkSummary([])
        with mock.patch.object(db, "run_park_pass", fake_pass), \
                mock.patch.object(db, "prune_facts", lambda *a: cold.PruneFacts(gone={self.lanes[0]})):
            rc, out = self.call_park_run(env, lock, auto=False)
        self.assertEqual(rc, 0, out)
        self.assertTrue(lock.held)
        self.assertEqual(len(self.store.records()), 2, out)
        self.assertIsNone(asked["floor_free"], "a by-hand pass has no comfort line to keep")
        lock.release()

    def test_a_recover_that_raises_is_said_and_the_pass_goes_on(self):
        env, lock = self.lock_and_env()
        with mock.patch.object(self.store, "recover", side_effect=OSError("device not configured")), \
                mock.patch.object(db, "run_park_pass", lambda *a, **k: db.ParkSummary([])):
            rc, out = self.call_park_run(env, lock)
        self.assertEqual(rc, 0, out)
        self.assertIn("recover failed: device not configured", out)
        lock.release()

    def test_prune_facts_name_a_merged_lane_and_a_gone_checkout_from_the_records(self):
        self.run_pass()
        merged_lane = self.lane("landed")
        cargo_tree(os.path.join(merged_lane, "rust-modules", "target"), size=64 * 1024, hours=9)
        gone = os.path.join(self.tmp, "vanished")
        for name, checkout in (("landed", merged_lane), ("vanished", gone)):
            self.store.records()                                   # keep the index readable
            cold.write_json(self.store.index_path(os.path.join(checkout, "rust-modules", "target")),
                            {"origin": os.path.join(checkout, "rust-modules", "target"), "checkout": checkout,
                             "parked_at": NOW_REAL, "apparent_bytes": 1, "id": name})
        facts = db.prune_facts(cold, self.store, self.main, self.env)
        self.assertIn(merged_lane, facts.merged)
        self.assertNotIn(self.lanes[0], facts.merged, "an unmerged lane is not merged")
        self.assertIn(gone, facts.gone)
        self.assertNotIn(self.lanes[0], facts.gone)


class WaitingFlag(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="disk-budget-flag-")
        self.addCleanup(tmp.cleanup)
        self.lock = os.path.join(tmp.name, "gc.lock")

    def test_an_announced_waiter_is_seen_and_a_withdrawn_one_is_not(self):
        self.assertFalse(db.preflight_waiting(self.lock))
        db.announce_waiting(self.lock)
        self.assertTrue(db.preflight_waiting(self.lock))
        db.withdraw_waiting(self.lock)
        self.assertFalse(db.preflight_waiting(self.lock))

    def test_a_flag_of_a_dead_waiter_or_an_old_one_is_not_a_waiter(self):
        dead = subprocess.Popen(["true"])
        dead.wait()
        flag = "%s.waiting.%d" % (self.lock, dead.pid)
        with open(flag, "w") as fh:
            fh.write("%d\n" % dead.pid)
        self.assertFalse(db.preflight_waiting(self.lock), "its owner is gone")
        os.unlink(flag)
        db.announce_waiting(self.lock)
        old = NOW_REAL - 3600
        os.utime("%s.waiting.%d" % (self.lock, os.getpid()), (old, old))
        self.assertFalse(db.preflight_waiting(self.lock), "a flag nobody refreshed for an hour")

    def test_two_waiters_do_not_clear_each_others_flag(self):
        other = subprocess.Popen(["sleep", "30"])
        self.addCleanup(other.terminate)
        self.addCleanup(other.wait)
        with open("%s.waiting.%d" % (self.lock, other.pid), "w") as fh:
            fh.write("%d\n" % other.pid)
        db.withdraw_waiting(self.lock)                         # this process had none, and must not take theirs
        self.assertTrue(db.preflight_waiting(self.lock))


class ParkCommand(Repo):
    """The subcommands as processes: unconfigured they say so and touch nothing; `--auto` is silent."""

    def run_tool(self, *args, **env):
        e = clean_env()
        e.update(self.env)
        e.update(PYTHONDONTWRITEBYTECODE="1", HOME=self.tmp, PLX_COLD_STORE_CONFIG=os.path.join(self.tmp, "none.cfg"),
                 PLX_TV_CONFIG_DIR=os.path.join(self.tmp, "none"), PLX_GC_LOG=os.path.join(self.tmp, "gc.log"))
        e.update(env)
        return subprocess.run([sys.executable, SCRIPT, *args], capture_output=True, text=True, env=e)

    def test_park_says_the_cold_store_is_off_and_creates_nothing(self):
        before = snapshot_files(self.tmp)
        out = self.run_tool("park", "--root", self.main)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("not configured", out.stdout)
        self.assertEqual(changed(before, snapshot_files(self.tmp)), [])

    def test_park_auto_unconfigured_is_silent_and_exits_3(self):
        out = self.run_tool("park", "--auto", "--root", self.main, PLX_GC_TEST_FREE_KIB="1")
        self.assertEqual((out.returncode, out.stdout, out.stderr), (db.PARK_NOT_APPLICABLE, "", ""))

    def test_park_auto_configured_but_with_room_is_silent_and_exits_3(self):
        cfg = os.path.join(self.tmp, "on.cfg")
        with open(cfg, "w") as fh:
            fh.write("root=%s\npark_below_gib=40\n" % os.path.join(self.tmp, "das", "cold"))
        out = self.run_tool("park", "--auto", "--root", self.main, PLX_COLD_STORE_CONFIG=cfg,
                            PLX_GC_TEST_FREE_KIB=str(100 * 1024 * 1024))
        self.assertEqual((out.returncode, out.stdout), (db.PARK_NOT_APPLICABLE, ""), out.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "das")), "looked at the cold volume with room to spare")

    def test_park_auto_below_the_comfort_line_parks_nothing_and_says_why(self):
        # The ladder deletes in seconds; a park holds the lock for a copy to a slow disk while a build waits for room.
        cfg = os.path.join(self.tmp, "on.cfg")
        with open(cfg, "w") as fh:
            fh.write("root=%s\npark_below_gib=40\n" % os.path.join(self.tmp, "das", "cold"))
        out = self.run_tool("park", "--auto", "--root", self.main, PLX_COLD_STORE_CONFIG=cfg,
                            PLX_GC_TEST_FREE_KIB=str(10 * 1024 * 1024), PLX_GC_MIN_FREE_GIB="20")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("park: skipped", out.stdout)
        self.assertIn("comfort line", out.stdout)
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "das")), "looked at the cold volume under pressure")
        self.assertFalse(os.path.exists(os.path.join(self.env["PLX_BUILD_CACHE"], "cold-store")))

    def test_a_pretend_emergency_over_the_default_cold_store_config_is_refused(self):
        config_home = os.path.join(self.tmp, "xdg")
        os.makedirs(os.path.join(config_home, "plxnative"))
        with open(os.path.join(config_home, "plxnative", "cold-store"), "w") as fh:
            fh.write("root=%s\n" % os.path.join(self.tmp, "das", "cold"))
        env = {**clean_env(), **self.env, "PLX_GC_TEST_FREE_KIB": "1", "HOME": self.tmp,
               "PYTHONDONTWRITEBYTECODE": "1", "XDG_CONFIG_HOME": config_home}
        out = subprocess.run([sys.executable, SCRIPT, "park", "--auto", "--root", self.main], capture_output=True,
                             text=True, env=env)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("PLX_COLD_STORE_CONFIG", out.stderr)

    def test_park_candidates_emit_lists_what_a_pass_would_take(self):
        path = os.path.join(self.tmpr, "ours")
        cargo_tree(path, hours=30)
        out = self.run_tool("park-candidates", "--emit", "--root", self.main, "--idle-min", "60",
                            PLX_GC_TEST_NOW=str(NOW_REAL + 3 * HOUR))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.split("\n")[0], "# plx-park-candidates v1")
        rows = [l.split("\t") for l in out.stdout.split("\n") if l.startswith("park\t")]
        self.assertEqual([r[5] for r in rows], [db.real(path)], out.stdout)
        self.assertEqual(rows[0][1], "tmp")

    def test_the_report_names_the_cold_store_state_and_changes_nothing(self):
        before = snapshot_files(self.tmp)
        off = self.run_tool("report", "--root", self.main)
        self.assertEqual(off.returncode, 0, off.stderr)
        self.assertRegex(off.stdout, r"cold store\s+off: not configured")
        self.assertEqual(changed(before, snapshot_files(self.tmp)), [])
        cfg = os.path.join(self.tmp, "on.cfg")
        with open(cfg, "w") as fh:
            fh.write("root=%s\n" % os.path.join(self.tmp, "das", "cold"))
        cache = self.env["PLX_BUILD_CACHE"]
        index = os.path.join(cache, "cold-store", "index")
        residue = os.path.join(cache, "cold-store", "parking", "%2Ftmp%2Fresidue")
        os.makedirs(index)
        os.makedirs(residue)
        with open(os.path.join(residue, "f"), "w") as fh:
            fh.write("x" * 5000)
        with open(os.path.join(index, "%2Ftmp%2Fa.json"), "w") as fh:
            fh.write('{"origin": "/tmp/a", "apparent_bytes": 3221225472, "parked_at": 1}')
        before = snapshot_files(self.tmp)
        on = self.run_tool("report", "--root", self.main, PLX_COLD_STORE_CONFIG=cfg)
        self.assertEqual(on.returncode, 0, on.stderr)
        self.assertRegex(on.stdout, r"cold store\s+on: root \S+/das/cold")
        self.assertIn("1 tree, 3.0 GiB parked", on.stdout)
        self.assertIn("parking residue", on.stdout)
        self.assertNotIn("restoring", on.stdout)
        self.assertEqual(changed(before, snapshot_files(self.tmp)), [], "the report touched the cold store")
        gone = subprocess.Popen(["true"])
        gone.wait()
        with open(os.path.join(index, "%%2Ftmp%%2Fb.json.unparking.%d" % gone.pid), "w") as fh:     # a restore that died
            fh.write('{"origin": "/tmp/b", "apparent_bytes": 1, "parked_at": 1}')
        claimed = self.run_tool("report", "--root", self.main, PLX_COLD_STORE_CONFIG=cfg)
        self.assertIn("1 tree, 3.0 GiB parked", claimed.stdout, "a claimed record is no longer parked")
        self.assertRegex(claimed.stdout, r"restoring: /tmp/b \(pid %d, dead: " % gone.pid)
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "das")), "the report touched the cold volume")


if __name__ == "__main__":
    unittest.main()
