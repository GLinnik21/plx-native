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
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
