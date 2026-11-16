#!/usr/bin/env python3
"""The disk budget at every build entry (docs/disk-budget.md, "The preflight"): `make disk-preflight` is
`tools/disk-budget.py preflight`, and it is an ORDER-ONLY prerequisite of every Makefile target that runs cargo.

Two halves.

STATIC. A build entry that is not hooked is a build that starts on a full disk and fails with ENOSPC deep
inside rustc, which reads like a code failure. So nothing here names today's target list: the Makefile is
parsed, every target whose recipe invokes cargo (directly, or through a script that does) is found, and each
must be in `make -s print-disk-entries`, the one list the `disk-preflight` rule is written over. A new
cargo-running target fails here until it is hooked; a script that mentions cargo must be classified in
DRIVERS or NOT_BUILDS below, so the detector cannot go quiet on a driver it does not know. The prerequisite
must be ORDER-ONLY (after the `|`), or a phony would make every file target out of date on every run.

DYNAMIC. The preflight as a process, against a fixture repository with the tools copied in (the executor
`build-gc.sh --evict` is the checkout it runs from, and it spares that checkout), every root pointed at a
temp directory, `PLX_GC_TEST_FREE_KIB` standing in for the disk, and `lsof`/`pgrep` stubbed:

* at or above comfort it is one statvfs and nothing else: no subprocess, and not even the imports the
  planner needs;
* below the reserve with nothing it may evict it refuses (exit 1, with the message that says why);
* below comfort with idle trees about it evicts them, never the tree it is building or the current checkout's other
  trees (bar their incremental dirs at rung 6), and goes on;
* `PLX_DISK_BUDGET_DONE=1` (a recursive make) and `PLX_MIN_FREE_MB=0` (the old escape hatch) are no-ops, bar
  one thing that comes first: a tree that is ABSENT with a parked copy in the cold store is unparked (never
  failing the build, never loading `cold-store.py` for a tree that is there, for no entry or with
  `PLX_COLD_STORE=off`): the cold store's own no-gos (no config, volume not mounted) end it before any lock is
  waited for, build-gc's auto lock is held to CLAIM the parked copy and not across the copy, and a tree another
  process is restoring is named, not copied again;
* it takes build-gc's auto lock; on a LIVE holder it waits a bounded time (one `--wait-secs` for the whole
  preflight, the unpark's wait included) and makes no plan while that runs
  (when the holder finishes or its lock goes stale it takes the lock over and plans; at the deadline it judges
  on free space alone);
* it never plans a pretend emergency over the real trees.

Nothing here builds: `make -n` and `make -s print-*` only, and no cargo.
"""
import contextlib
import importlib.util
import io
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from test_disk_budget import Repo, age_all, cargo_tree, clean_env, db, git_has_merge_tree, write  # noqa: E402

SCRIPT = os.path.join(ROOT, "tools", "disk-budget.py")
GIB = db.GIB


# --- the Makefile, read as text (never `make -p`) ------------------------------------------------------
class Rule:
    def __init__(self, targets, prereqs, order_only, recipe, line):
        self.targets, self.prereqs, self.order_only, self.recipe, self.line = targets, prereqs, order_only, recipe, line


ASSIGNMENT = re.compile(r"^[A-Za-z0-9_.$()\-]+\s*[:?+!]?=")
DIRECTIVE = re.compile(r"^(export|unexport|override|define|endef|ifeq|ifneq|ifdef|ifndef|else|endif|include|-include|vpath)\b")
TARGET_VAR = re.compile(r":\s*[A-Za-z0-9_$()\-]+\s*[:?+!]?=")


def parse_makefile(text):
    """Every rule header with its recipe: (targets, normal prerequisites, order-only prerequisites)."""
    rules, current = [], None
    lines = text.replace("\\\n", " ").split("\n")
    for number, raw in enumerate(lines, 1):
        if raw.startswith("\t"):
            if current is not None:
                current.recipe += raw[1:] + "\n"
            continue
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if DIRECTIVE.match(stripped):
            continue                                  # a conditional inside a recipe leaves the recipe open
        if ASSIGNMENT.match(stripped) or TARGET_VAR.search(stripped):
            current = None
            continue
        head, sep, rest = stripped.partition(":")
        if not sep:
            current = None
            continue
        normal, _, ordered = rest.partition("|")
        current = Rule(head.split(), normal.split(), ordered.split(), "", number)
        rules.append(current)
    return rules


CARGO = re.compile(r"\bcargo\b(?:\s+\+\S+)?\s+(?:build|rustc|test|check|clippy|run|bench|doc|fix)\b")
SCRIPT_REF = re.compile(r"(?:\.\./|\./)?((?:tools|ci|tests)/[\w./-]+\.(?:py|sh))")
CARGO_IN_SCRIPT = re.compile(r"""["']cargo["']|\bcargo\s+(?:\+\S+\s+)?(?:build|rustc|test|check|clippy|run|bench|doc|fix)\b""")

# Scripts a recipe names that run cargo: the target that names one is a build entry.
DRIVERS = {
    "tools/test-crate.py": "make test-crate: `cargo test` for one crate, in the test-fast tree",
    "tools/build-bench.py": "make build-bench: times the host build",
    "ci/mkmacapp.py": "make macapp / macapp-zip: a release cargo build of the simulator",
    "tools/fuzz-hud.py": "make fuzz-hud: `cargo fuzz build` of the fuzz package, in the target-fuzz tree",
}
# Scripts that mention cargo without building with it. The reason is the point: the detector stays loud.
NOT_BUILDS = {
    "ci/deps.py": "reads and edits the pinned dependency list; `cargo tree` / `metadata` only",
    "ci/stage-link-evidence.py": "copies the linker's sidecar files; cargo appears in prose",
    "ci/gen-third-party-notices.py": "renders notices from Cargo.lock; never starts a build",
    "tools/disk-budget.py": "the preflight itself",
    "tools/build-gc.sh": "the reclaim tool: it names cargo as a process to wait for, never to run",
}


def entry_names(make_out):
    return [line.strip() for line in make_out.splitlines() if line.strip()]


def run_make(*args, env=None):
    full = dict(os.environ)
    for name in ("MAKEFLAGS", "MFLAGS", "PLX_DISK_BUDGET_DONE"):
        full.pop(name, None)
    full.update(env or {})
    return subprocess.run(["make", *args], cwd=ROOT, env=full, capture_output=True, text=True)


class StaticWiring(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(os.path.join(ROOT, "Makefile")) as fh:
            cls.text = fh.read()
        cls.rules = parse_makefile(cls.text)
        out = run_make("-s", "print-disk-entries")
        assert out.returncode == 0, out.stderr
        cls.entries = entry_names(out.stdout)
        config = run_make("-s", "print-bench-config")
        assert config.returncode == 0, config.stderr
        cls.config = dict(line.split("=", 1) for line in config.stdout.splitlines() if "=" in line)

    ALIASES = {"$(RUST_LIB)": "RUST_LIB"}

    def soft(self, target):
        """A rule header's target as `print-disk-entries` spells it (the ARM staticlib is the one
        variable-named target that can be an entry); any other `$(...)` stays as written."""
        return self.config[self.ALIASES[target]] if target in self.ALIASES else target

    def resolve(self, target):
        """The same, for a target that RUNS CARGO: a spelling this test cannot resolve fails loudly."""
        if "$(" in target:
            self.assertIn(target, self.ALIASES, "teach test_disk_preflight.py how to resolve the Makefile target %s" % target)
        return self.soft(target)

    def builders(self):
        """{resolved target: why} for every rule whose recipe runs cargo, directly or through a script."""
        found = {}
        for rule in self.rules:
            if not rule.recipe:
                continue
            why = None
            if CARGO.search(rule.recipe):
                why = "runs cargo"
            # A target that runs cargo itself is an entry already; the scripts it names are its business.
            for ref in ([] if why else SCRIPT_REF.findall(rule.recipe)):
                if ref in DRIVERS:
                    why = why or "drives cargo through " + ref
                    continue
                path = os.path.join(ROOT, ref)
                if ref in NOT_BUILDS or not os.path.isfile(path):
                    continue
                with open(path, errors="replace") as fh:
                    if CARGO_IN_SCRIPT.search(fh.read()):
                        self.fail("%s (line %d) runs %s, which mentions cargo: add it to DRIVERS (a build entry, "
                                  "hook the target) or to NOT_BUILDS with the reason it never builds" % (
                                      " ".join(rule.targets), rule.line, ref))
            if why:
                for target in rule.targets:
                    found[self.resolve(target)] = why
        return found

    def test_the_detector_finds_the_builds_it_must(self):
        # A parser that stopped matching would pass every other test here with an empty set.
        builders = self.builders()
        for target in ("lint", "sim-macos", "test-fast", "check-ass", "check-ffmpeg", "site-video-sim",
                       "pkg/plxnative-storage", self.config["RUST_LIB"], "test-crate", "macapp", "build-bench"):
            self.assertIn(target, builders, "the detector no longer sees %s as a build" % target)

    def test_every_cargo_running_target_is_a_disk_entry(self):
        missing = {t: why for t, why in self.builders().items() if t not in self.entries}
        self.assertEqual(missing, {}, "these targets run cargo but are not in `make -s print-disk-entries`: add "
                                      "them to DISK_ENTRIES in the Makefile (the disk-preflight rule is written "
                                      "over that list)")

    def test_check_and_replay_are_entries_too(self):
        # `check` runs cargo through a lock wrapper and a recursive make, `check-replay` through sim-macos.
        for target in ("check", "check-replay"):
            self.assertIn(target, self.entries)

    def test_every_entry_is_a_real_target(self):
        headers = {self.soft(t) for r in self.rules for t in r.targets}
        for entry in self.entries:
            self.assertIn(entry, headers, "%s is in DISK_ENTRIES but no rule defines it" % entry)
        self.assertEqual(len(self.entries), len(set(self.entries)), "a target listed twice")

    def test_the_prerequisite_is_order_only_everywhere(self):
        mentions = [r for r in self.rules if r.targets != [".PHONY"] and "disk-preflight" in r.prereqs + r.order_only]
        self.assertTrue(mentions, "no rule makes disk-preflight a prerequisite")
        for rule in mentions:
            self.assertNotIn("disk-preflight", rule.prereqs,
                             "line %d makes disk-preflight a NORMAL prerequisite of %s: a phony would make a file "
                             "target out of date on every run; put it after the `|`" % (rule.line, rule.targets))
        wired = [r for r in mentions if r.targets == ["$(DISK_ENTRIES)"]]
        self.assertEqual(len(wired), 1, "exactly one rule hooks the list: `$(DISK_ENTRIES): | disk-preflight`")

    def test_make_n_runs_the_preflight_before_the_build(self):
        # The recursive entries (`check`, `install`) re-enter make even under -n; they are covered by the
        # text checks above. Everything else shows its recipe.
        recursive = {t for r in self.rules if "$(MAKE)" in r.recipe for t in map(self.soft, r.targets)}
        for entry in self.entries:
            if entry in recursive:
                continue
            with self.subTest(entry=entry):
                out = run_make("-n", entry)
                self.assertEqual(out.returncode, 0, out.stderr)
                lines = out.stdout.splitlines()
                pre = [i for i, l in enumerate(lines) if "disk-budget.py preflight" in l]
                self.assertEqual(len(pre), 1, "the preflight should run once, before the build:\n" + out.stdout)
                for i, l in enumerate(lines):
                    if CARGO.search(l):
                        self.assertLess(pre[0], i, "cargo is started before the preflight:\n" + out.stdout)

    def test_the_recipe_is_the_planner_not_df(self):
        (rule,) = [r for r in self.rules if r.targets == ["disk-preflight"] and r.recipe]
        self.assertIn("tools/disk-budget.py preflight", rule.recipe)
        self.assertIn("--tree", rule.recipe)
        self.assertNotIn("df -", rule.recipe)

    def test_a_recursive_make_skips_the_second_preflight(self):
        # The environment marker is set on every recipe line that re-enters make for a hooked target. A
        # child make that ran its own preflight would plan twice, and take a lock the parent may hold.
        needs = {"check": None, "install": None}
        for rule in self.rules:
            if any(self.soft(t) in self.entries or t in needs for t in rule.targets):
                for line in rule.recipe.splitlines():
                    if "$(MAKE)" in line:
                        self.assertIn("PLX_DISK_BUDGET_DONE=1", line,
                                      "%s re-enters make without the DONE marker: %s" % (rule.targets, line))
                        if rule.targets[0] in needs:
                            needs[rule.targets[0]] = line
        self.assertTrue(all(needs.values()), "check and install must both re-enter make with the marker: %s" % needs)

    def test_the_query_and_the_phony_are_side_effect_free(self):
        # `make print-disk-entries` / `make disk-preflight` must not reset the build stamp (and so delete
        # pkg/plxnative) the way a goal that builds does.
        joined = self.text.replace("\\\n", " ")
        self.assertRegex(joined, r"(?m)^QUERY_GOALS\s*=[^\n]*\bprint-disk-entries\b")
        self.assertRegex(joined, r"(?m)^SIDE_EFFECT_FREE\s*=[^\n]*\bdisk-preflight\b")


# --- the preflight, as a process ------------------------------------------------------------------------
@unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
class Preflight(Repo):
    TOOLS = ("disk-budget.py", "build-gc.sh", "cargo-seed.py", "check-lock.py")
    SCRUBBED = ("PLX_DISK_BUDGET_DONE", "PLX_MIN_FREE_MB", "PLX_GC_MIN_FREE_GIB", "PLX_GC_TEST_FREE_KIB",
                "PLX_GC_TEST_NOW", "PLX_DISK_NEED_GIB", "PLX_GC_LOCK_STALE_MIN", "CARGO_TARGET_DIR", "SIM_TDIR",
                "MAKEFLAGS", "MFLAGS")

    def setUp(self):
        super().setUp()
        self.scratch = os.path.join(self.tmp, "scratch")
        self.bin = os.path.join(self.tmp, "bin")
        self.lock = os.path.join(self.tmp, "gc.lock")
        for d in (self.scratch, self.bin):
            os.makedirs(d)
        self.env.update(PLX_GC_LOCK_DIR=self.lock, TMPDIR=self.scratch, PLX_GC_LOG=os.path.join(self.tmp, "gc.log"))
        self.stub("lsof", "printf 'p1\\ncsh\\nfcwd\\nn/\\n'\nexit 1\n")
        self.stub("pgrep", '[ "$1" = -x ] && exit 1\nexec %s "$@"\n' % shlex.quote(shutil.which("pgrep") or "/usr/bin/pgrep"))
        self.install_tools(self.main)

    def stub(self, name, body, directory=None):
        path = os.path.join(directory or self.bin, name)
        with open(path, "w") as fh:
            fh.write("#!/bin/sh\n" + body)
        os.chmod(path, 0o755)

    def install_tools(self, checkout):
        os.makedirs(os.path.join(checkout, "tools"), exist_ok=True)
        for name in self.TOOLS:
            shutil.copy(os.path.join(ROOT, "tools", name), os.path.join(checkout, "tools", name))

    def environment(self, free_gib=None, path_dir=None, **extra):
        env = clean_env()
        for name in self.SCRUBBED:
            env.pop(name, None)
        env.update(self.env)
        env.update(PATH=(path_dir or self.bin) + os.pathsep + env.get("PATH", ""), HOME=self.tmp,
                   PYTHONDONTWRITEBYTECODE="1", PLX_DISK_NEED_GIB="1")
        if free_gib is not None:
            env["PLX_GC_TEST_FREE_KIB"] = str(int(free_gib * 1024 * 1024))
        env.update(extra)
        return env

    def run_preflight(self, checkout=None, tree=None, free_gib=None, args=(), python_args=(), path_dir=None, **extra):
        checkout = checkout or self.main
        tree = tree or os.path.join(checkout, "rust-modules", "target")
        cmd = [sys.executable, *python_args, os.path.join(checkout, "tools", "disk-budget.py"), "preflight",
               "--tree", tree, *args]
        return subprocess.run(cmd, cwd=checkout, capture_output=True, text=True, timeout=180,
                              env=self.environment(free_gib, path_dir=path_dir, **extra))

    def logging_stubs(self):
        """Every tool a slow path would start, each recording that it was started."""
        directory = os.path.join(self.tmp, "logbin")
        os.makedirs(directory)
        log = os.path.join(self.tmp, "started.log")
        for name in ("git", "lsof", "sh", "bash", "pgrep", "ps", "make", "cargo", "python3", "find", "du", "df", "mkdir"):
            self.stub(name, 'echo "%s $*" >> %s\nexit 0\n' % (name, shlex.quote(log)), directory)
        return directory, log

    def started(self, log):
        return open(log).read() if os.path.exists(log) else ""

    # -- the fast path -------------------------------------------------------------------------------------
    def test_at_comfort_it_is_one_statvfs_and_starts_nothing(self):
        directory, log = self.logging_stubs()
        out = self.run_preflight(free_gib=40, path_dir=directory)
        self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""))
        self.assertEqual(self.started(log), "", "the fast path started a subprocess")

    def test_at_comfort_the_planner_is_not_even_imported(self):
        out = self.run_preflight(free_gib=40, python_args=("-X", "importtime"))
        self.assertEqual(out.returncode, 0, out.stderr)
        imported = {line.rsplit("|", 1)[-1].strip() for line in out.stderr.splitlines() if "|" in line}
        for module in ("subprocess", "tempfile", "ctypes", "argparse", "dataclasses"):
            self.assertNotIn(module, imported, "the fast path imported %s" % module)

    def test_the_real_free_space_is_read_when_nothing_overrides_it(self):
        directory, log = self.logging_stubs()
        out = self.run_preflight(path_dir=directory, PLX_GC_MIN_FREE_GIB="0", PLX_MIN_FREE_MB="1",
                                 PLX_DISK_NEED_GIB="0.001")
        self.assertEqual((out.returncode, out.stderr), (0, ""))
        self.assertEqual(self.started(log), "")

    def test_the_recursive_marker_makes_it_a_no_op(self):
        directory, log = self.logging_stubs()
        out = self.run_preflight(free_gib=0.001, path_dir=directory, PLX_DISK_BUDGET_DONE="1")
        self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""))
        self.assertEqual(self.started(log), "")

    def test_a_zero_reserve_is_the_old_escape_hatch(self):
        out = self.run_preflight(free_gib=0.001, PLX_MIN_FREE_MB="0")
        self.assertEqual((out.returncode, out.stderr), (0, ""))

    # -- unpark: the cold store brings the tree back before anything else is decided -----------------------
    # The copy of cold-store.py that runs next to the script under test gets a second "volume" (anything under
    # das/), a plain tree copy and tripwires: a marker line when the module is loaded and when `unpark` runs
    # (and whether build-gc's auto lock was held at that moment), a switch that makes the copy fail and one that
    # makes `unpark` raise.
    COLD_STUB = '''

# ---- appended by ci/test_disk_preflight.py: another volume under das/, a tree copy, and tripwires ----
_real_device_of = device_of


def device_of(path):
    return 2 if norm(path).startswith(%r) else _real_device_of(path)


def which_ditto():
    return "/usr/bin/ditto"


def copy_tree(src, dst):
    if os.environ.get("PLX_TEST_COLD_COPY_FAIL"):
        raise OSError("the cold volume went away")
    shutil.copytree(src, dst, symlinks=True)
    suffix, _, secs = os.environ.get("PLX_TEST_COLD_COPY_SLOW", "").partition(":")     # "<id suffix>:<seconds>"
    if suffix and src.endswith(suffix):
        time.sleep(float(secs))             # the copy is on disk and the cold volume is still "busy"


_marker = os.environ.get("PLX_TEST_COLD_MARKER")
if _marker:
    with open(_marker, "a") as _fh:
        _fh.write("loaded\\n")

_real_unpark = ColdStore.unpark
_real_claim = ColdStore.claim


def _note(what):
    if _marker:
        with open(_marker, "a") as _fh:
            _fh.write("%%s lock=%%d\\n" %% (what, os.path.isdir(os.environ.get("PLX_GC_LOCK_DIR", ""))))


def _claim(self, *args, **kwargs):
    _note("claim")
    return _real_claim(self, *args, **kwargs)


def _unpark(self, *args, **kwargs):
    _note("unpark")
    if os.environ.get("PLX_TEST_COLD_RAISE"):
        raise RuntimeError("boom")
    return _real_unpark(self, *args, **kwargs)


ColdStore.claim = _claim
ColdStore.unpark = _unpark
'''

    def main_tree(self):
        return os.path.join(self.main, "rust-modules", "target")

    def cold_fixture(self, parked=True, config=""):
        """A cold store the preflight can use: a config, a das/ "volume" and the stubbed cold-store.py in the
        fixture checkout. With `parked` the main checkout's `rust-modules/target` has been parked (it is absent
        and the SSD index holds its entry); without, the tree is absent and nothing is indexed."""
        das = os.path.join(self.tmp, "das")
        self.cold_root = os.path.join(das, "cold")
        os.makedirs(self.cold_root)
        self.cold_marker = os.path.join(self.tmp, "cold-loaded")
        self.cold_module = os.path.join(self.main, "tools", "cold-store.py")
        with open(os.path.join(ROOT, "tools", "cold-store.py")) as src, open(self.cold_module, "w") as dst:
            dst.write(src.read() + self.COLD_STUB % das)
        cfg = os.path.join(self.tmp, "cold-store.cfg")
        with open(cfg, "w") as fh:
            fh.write("root=%s\nmin_free_gib=0\n%s" % (self.cold_root, config))
        self.env.update(PLX_COLD_STORE_CONFIG=cfg, PLX_TEST_COLD_MARKER=self.cold_marker)
        self.cold_config = cfg
        spec = importlib.util.spec_from_file_location("cold_stub", self.cold_module)
        self.cold_mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.cold_mod)
        if parked:
            self.park_fixture(self.main_tree())
            self.assertEqual(len(self.parked_entries()), 1)

    def park_fixture(self, tree):
        """Build a cargo tree at `tree` and park it: it is absent afterwards and the SSD index holds its entry."""
        cargo_tree(tree)
        store = self.cold_mod.ColdStore({"PLX_BUILD_CACHE": self.cache, "PLX_COLD_STORE_CONFIG": self.cold_config,
                                         "HOME": self.tmp}, log=lambda m: None)
        out = store.park(tree, kind="lane", checkout=self.main, head="", live=None)
        self.assertTrue(out.ok, out)
        self.assertFalse(os.path.exists(tree), "the fixture's tree is still in place")

    def claim_files(self):
        index = os.path.join(self.cache, "cold-store", "index")
        return sorted(n for n in os.listdir(index) if ".unparking." in n) if os.path.isdir(index) else []

    def entry_path(self, tree):
        return os.path.join(self.cache, "cold-store", "index", db.cold_index_name(tree) + ".json")

    def foreign_claim(self, tree, pid):
        """The state another process leaves while it restores `tree`: its index entry renamed to its pid's claim."""
        os.rename(self.entry_path(tree), "%s.unparking.%d" % (self.entry_path(tree), pid))

    def lines(self, out):
        return [l for l in out.stderr.splitlines() if "cold-store:" in l]

    def parked_entries(self):
        index = os.path.join(self.cache, "cold-store", "index")
        return sorted(os.listdir(index)) if os.path.isdir(index) else []

    def cold_marker_text(self):
        if not os.path.exists(self.cold_marker):
            return ""
        with open(self.cold_marker) as fh:
            return fh.read()

    def cold_loaded(self):
        return "loaded" in self.cold_marker_text()

    def assert_unparked(self, out):
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertRegex(out.stderr, r"cold-store: unpark [\d.]+ \w+ in [\d.]+ s: " + re.escape(self.main_tree()))
        self.assertTrue(os.path.isfile(os.path.join(self.main_tree(), "CACHEDIR.TAG")), "the tree did not come back")
        self.assertEqual(self.parked_entries(), [], "the index still lists a tree that is back")
        text = self.cold_marker_text()
        self.assertIn("claim lock=1", text, "the parked copy was claimed without build-gc's auto lock")
        self.assertIn("unpark lock=0", text, "the copy ran with build-gc's auto lock held")
        self.assertEqual(self.claim_files(), [], "a claim outlived the restore")
        self.assertFalse(os.path.exists(self.lock), "the auto lock was not released")
        self.assertEqual(self.flags(), [])

    def assert_cold_tree_left_alone(self, out):
        """The preflight went on as it would have with no cold store: the tree is still absent and still parked."""
        self.assertFalse(os.path.exists(self.main_tree()), "a tree appeared")
        self.assertEqual(len(self.parked_entries()), 1, "the parked copy was dropped")
        self.assertFalse(os.path.exists(self.lock), "the auto lock was left behind")
        self.assertEqual([n for n in os.listdir(os.path.dirname(self.main_tree())) if "unpark" in n], [],
                         "a half-copied sibling was left beside the tree")

    def test_an_absent_tree_with_a_parked_copy_is_claimed_under_the_auto_lock_and_copied_without_it(self):
        self.cold_fixture()
        self.assert_unparked(self.run_preflight(free_gib=40))

    def test_it_unparks_even_with_the_recursive_marker_set(self):
        # `make check`, `install` and `build-bench` run their children with PLX_DISK_BUDGET_DONE=1, and each child
        # still has its own tree: the check sits before the marker's exit.
        self.cold_fixture()
        self.assert_unparked(self.run_preflight(free_gib=40, PLX_DISK_BUDGET_DONE="1"))

    def test_it_unparks_even_with_a_zero_reserve(self):
        self.cold_fixture()
        self.assert_unparked(self.run_preflight(free_gib=40, PLX_MIN_FREE_MB="0"))

    def test_it_unparks_on_the_full_path_too_and_goes_on_into_the_ordinary_decision(self):
        # A build bigger than the free space takes the full path (the planner): the tree still comes back first,
        # and the verdict is the one it would have been with no cold store.
        self.cold_fixture()
        args = ("--need-gib", "30")
        off = self.run_preflight(free_gib=25, args=args, PLX_COLD_STORE="off")
        self.assertFalse(os.path.exists(self.main_tree()))
        on = self.run_preflight(free_gib=25, args=args)
        self.assertEqual(on.returncode, off.returncode, on.stderr)
        self.assertRegex(on.stderr, r"cold-store: unpark")
        self.assertTrue(os.path.isfile(os.path.join(self.main_tree(), "CACHEDIR.TAG")))
        self.assertEqual(self.parked_entries(), [])

    def test_a_present_tree_never_loads_the_cold_store(self):
        self.cold_fixture()
        cargo_tree(self.main_tree())                    # a build recreated it: the parked copy is stale
        for extra in ({}, {"PLX_DISK_BUDGET_DONE": "1"}, {"PLX_MIN_FREE_MB": "0"}):
            out = self.run_preflight(free_gib=40, **extra)
            self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""), extra)
        self.assertFalse(self.cold_loaded(), "the cold store was loaded for a tree that is there")
        self.assertEqual(len(self.parked_entries()), 1, "the stale record is prune's to drop, not the preflight's")

    def test_an_absent_tree_with_no_index_entry_never_loads_the_cold_store(self):
        self.cold_fixture(parked=False)
        for extra in ({}, {"PLX_DISK_BUDGET_DONE": "1"}, {"PLX_MIN_FREE_MB": "0"}):
            out = self.run_preflight(free_gib=40, **extra)
            self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""), extra)
        self.assertFalse(self.cold_loaded())

    def test_the_off_switch_never_loads_the_cold_store(self):
        self.cold_fixture()
        for extra in ({}, {"PLX_DISK_BUDGET_DONE": "1"}):
            out = self.run_preflight(free_gib=40, PLX_COLD_STORE="off", **extra)
            self.assertEqual((out.returncode, out.stdout, out.stderr), (0, "", ""), extra)
        self.assertFalse(self.cold_loaded())
        self.assert_cold_tree_left_alone(out)

    def test_a_declined_unpark_leaves_the_build_to_the_ordinary_decision(self):
        self.cold_fixture(config="rebuild_secs_target=0\n")           # a rebuild is "free": nothing is worth copying
        for free in (40, 2):
            off = self.run_preflight(free_gib=free, PLX_COLD_STORE="off")
            on = self.run_preflight(free_gib=free)
            self.assertEqual(on.returncode, off.returncode, "the cold store changed the verdict at %s GiB: %s" % (free, on.stderr))
            self.assertIn("cold-store:", on.stderr)
            self.assertRegex(on.stderr, r"(?i)declined")
            self.assertEqual(sum("cold-store:" in l for l in on.stderr.splitlines()), 1, on.stderr)
            self.assert_cold_tree_left_alone(on)
        self.assertEqual(off.returncode, 1, "the 2 GiB case should be the refusal it always was: " + off.stderr)

    def test_a_failed_copy_prints_its_estimate_and_one_outcome_line_and_the_build_goes_on(self):
        self.cold_fixture()
        out = self.run_preflight(free_gib=40, PLX_TEST_COLD_COPY_FAIL="1")
        self.assertEqual(out.returncode, 0, out.stderr)
        lines = [l for l in out.stderr.splitlines() if "cold-store:" in l]
        self.assertEqual(len(lines), 2, out.stderr)
        self.assertEqual(sum("(est " in l for l in lines), 1, out.stderr)
        self.assertEqual(sum("not restored" in l for l in lines), 1, out.stderr)
        self.assertRegex(out.stderr, r"unpark failed")
        self.assert_cold_tree_left_alone(out)

    def test_an_exception_in_the_cold_store_prints_one_line_and_the_build_goes_on(self):
        self.cold_fixture()
        for extra in ({}, {"PLX_DISK_BUDGET_DONE": "1"}):
            out = self.run_preflight(free_gib=40, PLX_TEST_COLD_RAISE="1", **extra)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertEqual(sum("cold-store:" in l for l in out.stderr.splitlines()), 1, out.stderr)
            self.assertIn("RuntimeError", out.stderr)
            self.assert_cold_tree_left_alone(out)

    def test_a_cold_store_module_that_cannot_be_loaded_prints_one_line_and_the_build_goes_on(self):
        self.cold_fixture()
        with open(self.cold_module, "w") as fh:
            fh.write("this is not python\n")
        out = self.run_preflight(free_gib=40)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(sum("cold-store:" in l for l in out.stderr.splitlines()), 1, out.stderr)
        self.assert_cold_tree_left_alone(out)

    def test_a_held_auto_lock_is_waited_on_for_a_bounded_time_then_the_build_rebuilds_cold(self):
        self.cold_fixture()
        self.hold_lock()
        started = time.monotonic()
        out = self.run_preflight(free_gib=40, args=("--wait-secs", "2"))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertGreaterEqual(time.monotonic() - started, 1.5, "did not wait for the holder")
        self.assertEqual(sum("cold-store:" in l for l in out.stderr.splitlines()), 1, out.stderr)
        self.assertFalse(os.path.exists(self.main_tree()))
        self.assertEqual(len(self.parked_entries()), 1)
        self.assertTrue(os.path.isdir(self.lock), "took over somebody else's lock")
        self.assertEqual(self.flags(), [], "the waiting flag outlived the wait")
        self.assertNotIn("claim", self.cold_marker_text(), "claimed without the lock")
        self.assertNotIn("unpark", self.cold_marker_text(), "unparked without the lock")

    def test_the_unpark_wait_and_the_ordinary_wait_spend_one_deadline(self):
        # The lock is held throughout and the build does not fit: the unpark waits for it, then the ordinary decision
        # would wait for it again. One --wait-secs is all a build spends on the lock, not one per stage.
        self.cold_fixture()
        self.hold_lock()
        started = time.monotonic()
        out = self.run_preflight(free_gib=2, args=("--wait-secs", "3"))
        waited = time.monotonic() - started
        self.assertEqual(out.returncode, 1, out.stderr)
        self.assertIn("reclaim", out.stderr)
        self.assertGreaterEqual(waited, 2.5, "did not wait for the holder")
        self.assertLess(waited, 5.0, "the two lock waits each took the whole --wait-secs: %.1f s" % waited)
        self.assertEqual(len(self.lines(out)), 1, out.stderr)
        self.assertEqual(len(self.parked_entries()), 1)
        self.assertEqual(self.flags(), [])

    def assert_no_wait_no_lock_no_copy(self, out, waited, why):
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertLess(waited, 10, "waited for the lock although the cold store cannot be used")
        lines = self.lines(out)
        self.assertEqual(len(lines), 1, out.stderr)
        self.assertIn(why, lines[0])
        self.assertTrue(self.cold_loaded())
        self.assertNotIn("claim", self.cold_marker_text(), "tried to claim a copy the cold store cannot reach")
        self.assertFalse(os.path.exists(self.main_tree()))
        self.assertEqual(len(self.parked_entries()), 1, "the parked copy was dropped")
        self.assertTrue(os.path.isdir(self.lock), "took over somebody else's lock")
        self.assertEqual(self.flags(), [], "announced itself as a waiter for a lock it never needed")

    def test_a_cold_store_whose_config_was_removed_is_one_line_and_waits_for_nothing(self):
        self.cold_fixture()
        os.unlink(self.cold_config)
        self.hold_lock()
        started = time.monotonic()
        out = self.run_preflight(free_gib=40, args=("--wait-secs", "30"))
        self.assert_no_wait_no_lock_no_copy(out, time.monotonic() - started, "not configured")

    def test_a_cold_volume_that_is_not_mounted_is_one_line_and_waits_for_nothing(self):
        self.cold_fixture()
        with open(self.cold_config, "w") as fh:                   # the parked copy's disk is not in the machine
            fh.write("root=/Volumes/plx-test-%s/cold\nmin_free_gib=0\n" % os.path.basename(self.tmp))
        self.hold_lock()
        started = time.monotonic()
        out = self.run_preflight(free_gib=40, args=("--wait-secs", "30"))
        self.assert_no_wait_no_lock_no_copy(out, time.monotonic() - started, "not mounted")

    def test_two_trees_restore_at_once_and_the_copy_does_not_hold_the_global_lock(self):
        # `make check` runs two branches with different trees, each with PLX_DISK_BUDGET_DONE=1. The first one's copy
        # is slow; the second must neither wait for it nor give up on it: only the CLAIM runs under the lock.
        self.cold_fixture()
        fast = os.path.join(self.main, "rust-modules", "target-fast")
        self.park_fixture(fast)
        first = subprocess.Popen(
            [sys.executable, os.path.join(self.main, "tools", "disk-budget.py"), "preflight",
             "--tree", self.main_tree(), "--wait-secs", "2"],
            cwd=self.main, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            env=self.environment(free_gib=40, PLX_DISK_BUDGET_DONE="1", PLX_TEST_COLD_COPY_SLOW="%2Ftarget:8"))
        self.addCleanup(first.kill)
        siblings = lambda: [n for n in os.listdir(os.path.dirname(self.main_tree())) if "unpark" in n]
        seen = time.monotonic() + 60
        while not siblings() and first.poll() is None and time.monotonic() < seen:      # the copy is under way
            time.sleep(0.05)
        self.assertEqual(len(siblings()), 1, "the first build never started its copy")
        started = time.monotonic()
        out = self.run_preflight(tree=fast, free_gib=40, args=("--wait-secs", "2"), PLX_DISK_BUDGET_DONE="1")
        took = time.monotonic() - started
        self.assertIsNone(first.poll(), "the first copy was over already: this proves nothing")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertRegex(out.stderr, r"cold-store: unpark [\d.]+ \w+ in [\d.]+ s: " + re.escape(fast))
        self.assertTrue(os.path.isfile(os.path.join(fast, "CACHEDIR.TAG")), "the second tree waited out the first copy")
        self.assertLess(took, 6, "the second restore took %.1f s behind the first one's 8 s copy" % took)
        self.assertEqual(len(self.claim_files()), 1, "the first build's claim is not the one entry left")
        stderr = first.communicate(timeout=90)[1]
        self.assertEqual(first.returncode, 0, stderr)
        self.assertTrue(os.path.isfile(os.path.join(self.main_tree(), "CACHEDIR.TAG")))
        self.assertEqual(self.parked_entries(), [])
        self.assertFalse(os.path.exists(self.lock), "the auto lock was left behind")
        self.assertEqual(self.flags(), [])

    def test_a_tree_another_build_is_restoring_is_named_in_one_line_and_not_copied_again(self):
        self.cold_fixture()
        sleeper = subprocess.Popen(["sleep", "120"])
        self.addCleanup(sleeper.wait)
        self.addCleanup(sleeper.terminate)
        self.foreign_claim(self.main_tree(), sleeper.pid)
        out = self.run_preflight(free_gib=40)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(len(self.lines(out)), 1, out.stderr)
        self.assertIn("being restored by pid %d" % sleeper.pid, out.stderr)
        self.assertNotIn("unpark lock", self.cold_marker_text(), "copied a tree somebody else restores")
        self.assertFalse(os.path.exists(self.main_tree()))
        self.assertEqual(len(self.claim_files()), 1, "took the other build's claim")
        self.assertFalse(os.path.exists(self.lock), "the auto lock was left behind")

    def test_a_claimer_that_died_leaves_an_entry_the_next_build_restores(self):
        self.cold_fixture()
        gone = subprocess.Popen(["true"])
        gone.wait()
        self.foreign_claim(self.main_tree(), gone.pid)
        self.assert_unparked(self.run_preflight(free_gib=40))

    def test_an_exception_after_the_claim_gives_the_claim_back(self):
        self.cold_fixture()
        out = self.run_preflight(free_gib=40, PLX_TEST_COLD_RAISE="1")
        self.assertIn("RuntimeError", out.stderr)
        self.assertEqual(self.claim_files(), [], "the claim outlived the exception")
        self.assert_cold_tree_left_alone(out)
        self.assertEqual(self.parked_entries(), [os.path.basename(self.entry_path(self.main_tree()))])

    def test_a_copy_that_would_leave_less_than_comfort_free_is_declined_and_the_entry_stays(self):
        # The cost rule passes (a rebuild is slow, the copy tiny); the caller's free-space rule is the one that says no.
        self.cold_fixture()
        apparent = self.cold_mod.tree_stats(os.path.join(self.cold_root, "trees", self.cold_mod.escape(self.main_tree())))[1]
        self.assertGreater(apparent, 2048, "the fixture tree is too small for a KiB-granular free-space override")
        comfort_kib = 20 * 1024 * 1024
        just_short = comfort_kib + apparent // 1024 - 1                  # free - apparent < comfort by a few hundred bytes
        out = self.run_preflight(free_gib=None, PLX_GC_MIN_FREE_GIB="20", PLX_GC_TEST_FREE_KIB=str(just_short))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(len(self.lines(out)), 1, out.stderr)
        self.assertRegex(out.stderr, r"(?i)not restored \(declined\): free space after copying")
        self.assert_cold_tree_left_alone(out)
        self.assertEqual(self.claim_files(), [])
        just_enough = comfort_kib + apparent // 1024 + 1                 # the same tree, one KiB more room: it comes back
        out = self.run_preflight(free_gib=None, PLX_GC_MIN_FREE_GIB="20", PLX_GC_TEST_FREE_KIB=str(just_enough))
        self.assertRegex(out.stderr, r"cold-store: unpark [\d.]+ \w+ in [\d.]+ s: ")
        self.assertTrue(os.path.isfile(os.path.join(self.main_tree(), "CACHEDIR.TAG")))
        self.assertEqual(self.parked_entries(), [])

    def test_the_claim_name_the_fast_path_looks_for_is_cold_stores_own(self):
        spec = importlib.util.spec_from_file_location("cold_real_claim", os.path.join(ROOT, "tools", "cold-store.py"))
        cold = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cold)
        self.assertEqual(db.COLD_CLAIM_INFIX, cold.CLAIM_INFIX)
        tree = os.path.join(self.tmp, "t", "target")
        index = os.path.join(self.cache, "cold-store", "index")
        os.makedirs(index)
        env = {"PLX_BUILD_CACHE": self.cache}
        self.assertFalse(db.cold_unpark_pending(tree, env))
        write(os.path.join(index, db.cold_index_name(tree) + ".json.unparking.4242"), 4)
        self.assertTrue(db.cold_unpark_pending(tree, env), "a tree being restored is not a tree to rebuild cold unannounced")
        self.assertFalse(db.cold_unpark_pending(os.path.join(self.tmp, "t", "target-fast"), env))

    def test_the_index_name_the_fast_path_computes_is_cold_stores_own(self):
        spec = importlib.util.spec_from_file_location("cold_real", os.path.join(ROOT, "tools", "cold-store.py"))
        cold = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cold)
        for path in ("/a/b/rust-modules/target", "/with%percent/x%2Fy/target", "/Volumes/DAS/" + "deep/" * 60 + "target",
                     "/" + "d" * 223 + "/t", "/" + "d" * 224 + "/t"):
            self.assertEqual(db.cold_index_name(path), cold.escape(path), path)

    # -- refusing ------------------------------------------------------------------------------------------
    def test_below_the_reserve_with_nothing_to_evict_it_refuses_and_says_why(self):
        cargo_tree(os.path.join(self.main, "rust-modules", "target"), incr=1)      # the tree being built
        out = self.run_preflight(free_gib=2)
        self.assertEqual(out.returncode, 1, out.stderr)
        for text in ("disk-preflight:", "2.0 GiB", "3.0 GiB", "make disk", "PLX_MIN_FREE_MB=0"):
            self.assertIn(text, out.stderr)
        self.assertTrue(os.path.isdir(os.path.join(self.main, "rust-modules", "target", "debug", "incremental")))

    def test_the_refusal_names_the_trees_that_cannot_be_freed(self):
        import fcntl
        other = self.lane("other", files={"f.txt": "x"})
        busy_tree = os.path.join(other, "rust-modules", "target")
        cargo_tree(busy_tree)
        age_all(other, 5)
        lock = os.path.join(busy_tree, "debug", ".cargo-lock")
        write(lock, 1)
        fd = os.open(lock, os.O_RDWR)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        out = self.run_preflight(free_gib=2)
        self.assertEqual(out.returncode, 1, out.stderr)
        self.assertIn("kept (in use)", out.stderr)
        self.assertIn("other/rust-modules/target", out.stderr)     # printed with ~ for the home directory
        self.assertIn("cargo lock held", out.stderr)
        self.assertTrue(os.path.isdir(busy_tree))

    def test_the_pretend_emergency_guard_applies(self):
        # A free-space override over the DEFAULT roots would plan the owner's real trees.
        env = self.environment(free_gib=2)
        del env["PLX_FLEET_DIR"]
        out = subprocess.run([sys.executable, os.path.join(self.main, "tools", "disk-budget.py"), "preflight",
                              "--tree", os.path.join(self.main, "rust-modules", "target")],
                             cwd=self.main, capture_output=True, text=True, env=env)
        self.assertEqual(out.returncode, 2, out.stderr)
        self.assertIn("PLX_FLEET_DIR", out.stderr)

    def test_a_checkout_outside_the_named_roots_is_not_planned_under_an_override(self):
        # The roots are disposable, the checkout is not: `git worktree list` from a real one would hand the
        # planner every real lane. This one is a stand-in beside the fixture, on the SAME volume as the tree
        # it builds for, wherever TMPDIR is. (The real checkout would not do: on another volume than TMPDIR,
        # a checkout on /Volumes/DAS against the default TMPDIR, the preflight judges the tree on its own
        # free space and never reaches the guard under test. That path is
        # `test_a_tree_on_another_volume_is_judged_on_its_own_free_space_and_not_planned`.)
        checkout = os.path.realpath(tempfile.mkdtemp(prefix="standin-checkout-", dir=os.path.dirname(self.tmp)))
        self.addCleanup(shutil.rmtree, checkout, ignore_errors=True)
        self.assertEqual(os.stat(checkout).st_dev, os.stat(self.main).st_dev, "the stand-in must share the tree's volume")
        env = self.environment(free_gib=2)
        out = subprocess.run([sys.executable, SCRIPT, "preflight", "--root", checkout,
                              "--tree", os.path.join(self.main, "rust-modules", "target")],
                             cwd=self.main, capture_output=True, text=True, env=env)
        self.assertEqual(out.returncode, 2, out.stderr)
        self.assertIn("lies outside", out.stderr)

    # -- evicting ------------------------------------------------------------------------------------------
    def build_fleet(self):
        """The current lane, an unmerged idle lane, an orphan fleet tree and the main checkout's tree."""
        mine = self.lane("mine")
        self.install_tools(mine)
        other = self.lane("other", files={"f.txt": "x"})
        paths = {
            "mine": os.path.join(mine, "rust-modules", "target"),
            "mine_sim": os.path.join(mine, "rust-modules", "target-sim"),
            "other": os.path.join(other, "rust-modules", "target"),
            "orphan": os.path.join(self.fleet, "gone", "target"),
            "main": os.path.join(self.main, "rust-modules", "target"),
        }
        cargo_tree(paths["mine"], incr=2)
        cargo_tree(paths["mine_sim"], incr=1)
        cargo_tree(paths["other"], incr=2)
        cargo_tree(paths["orphan"])
        cargo_tree(paths["main"], incr=2)
        age_all(other, 5)
        return mine, other, paths

    def test_below_comfort_it_evicts_idle_trees_and_never_the_checkout_it_builds_for(self):
        mine, other, paths = self.build_fleet()
        out = self.run_preflight(checkout=mine, free_gib=10)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertFalse(os.path.exists(paths["orphan"]), "garbage should go first: " + out.stderr)
        self.assertFalse(os.path.exists(paths["other"]), "an idle lane's tree should go: " + out.stderr)
        for kept in ("mine", "mine_sim", "main"):
            self.assertTrue(os.path.isdir(paths[kept]), "evicted %s: %s" % (kept, out.stderr))
        for incr in (os.path.join(paths["mine"], "debug", "incremental"),
                     os.path.join(paths["mine_sim"], "debug", "incremental"),
                     os.path.join(paths["main"], "debug", "incremental")):
            self.assertTrue(os.path.isdir(incr), "evicted %s: %s" % (incr, out.stderr))
        self.assertIn("evicted", out.stderr)
        self.assertFalse(os.path.exists(self.lock), "the auto lock is released")

    def test_it_does_the_same_from_a_real_make_parent(self):
        # `make` is itself a running builder in the checkout being built: build-gc must not read that as a
        # reason to evict nothing, and must not treat that checkout as evictable.
        if not shutil.which("lsof") or not shutil.which("make"):
            self.skipTest("needs lsof and make")
        mine, other, paths = self.build_fleet()
        with open(os.path.join(mine, "Makefile"), "w") as fh:
            fh.write(".PHONY: x disk-preflight\nx: | disk-preflight\n\t@echo built\n"
                     "disk-preflight:\n\t@python3 tools/disk-budget.py preflight --tree '%s'\n" % paths["mine"])
        os.symlink(sys.executable, os.path.join(self.bin, "python3"))
        for name in ("lsof", "pgrep"):
            os.unlink(os.path.join(self.bin, name))          # the real ones: a real make has to be seen
        out = subprocess.run(["make", "x"], cwd=mine, capture_output=True, text=True, timeout=180,
                             env=self.environment(free_gib=10))
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("built", out.stdout)
        for kept in ("mine", "mine_sim", "main"):
            self.assertTrue(os.path.isdir(paths[kept]), "evicted %s: %s" % (kept, out.stderr))
        if "cannot say which checkout" in out.stderr + out.stdout:
            self.skipTest("this host cannot read a builder's working directory, so build-gc refuses by design")
        self.assertFalse(os.path.exists(paths["other"]), out.stderr)
        self.assertFalse(os.path.exists(paths["orphan"]), out.stderr)

    def test_an_executor_that_cannot_run_is_not_a_reason_to_refuse_a_build_that_fits(self):
        mine, _, paths = self.build_fleet()
        os.unlink(os.path.join(mine, "tools", "build-gc.sh"))
        fits = self.run_preflight(checkout=mine, free_gib=10)
        self.assertEqual(fits.returncode, 0, fits.stderr)
        self.assertIn("build-gc", fits.stderr)
        self.assertTrue(os.path.isdir(paths["orphan"]), "nothing deletes without the executor")
        short = self.run_preflight(checkout=mine, free_gib=2)
        self.assertEqual(short.returncode, 1, short.stderr)

    # -- the lock ------------------------------------------------------------------------------------------
    def hold_lock(self):
        proc = subprocess.Popen(["sleep", "120"])
        self.addCleanup(proc.wait)
        self.addCleanup(proc.terminate)
        os.makedirs(self.lock)
        with open(os.path.join(self.lock, "pid"), "w") as fh:
            fh.write("%d\n" % proc.pid)
        return proc

    def test_a_held_lock_is_not_waited_on_when_the_build_fits_and_no_plan_is_made(self):
        self.hold_lock()
        orphan = os.path.join(self.fleet, "gone", "target")
        cargo_tree(orphan)
        started = time.monotonic()
        out = self.run_preflight(free_gib=10, args=("--wait-secs", "30"))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertLess(time.monotonic() - started, 20, "waited although the build fits")
        self.assertTrue(os.path.isdir(orphan), "a preflight that did not hold the lock planned an eviction")
        self.assertTrue(os.path.isdir(self.lock), "took over somebody else's lock")

    def test_a_held_lock_is_waited_on_for_a_bounded_time_then_judged_on_free_space(self):
        self.hold_lock()
        cargo_tree(os.path.join(self.fleet, "gone", "target"))
        started = time.monotonic()
        out = self.run_preflight(free_gib=2, args=("--wait-secs", "2"))
        self.assertEqual(out.returncode, 1, out.stderr)
        self.assertGreaterEqual(time.monotonic() - started, 1.5, "did not wait for the holder")
        self.assertIn("reclaim", out.stderr)
        self.assertTrue(os.path.isdir(os.path.join(self.fleet, "gone", "target")))

    def flags(self):
        return sorted(n for n in os.listdir(os.path.dirname(self.lock)) if ".waiting." in n)

    def test_a_preflight_that_waits_flags_itself_beside_the_lock_and_withdraws_the_flag(self):
        # The cold store's park pass holds the lock for minutes and reads this flag between trees (docs/disk-budget.md,
        # "The cold store"): without it a build would wait out its --wait-secs behind a copy.
        self.hold_lock()
        cargo_tree(os.path.join(self.fleet, "gone", "target"))
        waiter = subprocess.Popen([sys.executable, os.path.join(self.main, "tools", "disk-budget.py"), "preflight",
                                   "--tree", os.path.join(self.main, "rust-modules", "target"), "--wait-secs", "4"],
                                  cwd=self.main, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                  env=self.environment(free_gib=2))
        flag = "%s.waiting.%d" % (self.lock, waiter.pid)
        deadline = time.monotonic() + 60
        while not os.path.exists(flag) and waiter.poll() is None and time.monotonic() < deadline:
            time.sleep(0.1)
        seen = os.path.exists(flag)
        waiter.communicate(timeout=90)
        self.assertTrue(seen, "a waiting preflight left no flag beside the lock")
        self.assertEqual(self.flags(), [], "the flag outlived the wait")

    def test_a_preflight_that_does_not_wait_leaves_no_flag(self):
        self.hold_lock()
        out = self.run_preflight(free_gib=10, args=("--wait-secs", "30"))        # fits: it never waits
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(self.flags(), [])

    def test_the_lock_dropped_during_the_wait_ends_it(self):
        proc = self.hold_lock()
        waiter = subprocess.Popen([sys.executable, os.path.join(self.main, "tools", "disk-budget.py"), "preflight",
                                   "--tree", os.path.join(self.main, "rust-modules", "target"), "--wait-secs", "60"],
                                  cwd=self.main, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                  env=self.environment(free_gib=2))
        time.sleep(1.5)
        proc.terminate()
        proc.wait()
        shutil.rmtree(self.lock)
        started = time.monotonic()
        _, stderr = waiter.communicate(timeout=90)
        self.assertLess(time.monotonic() - started, 30, "kept waiting after the holder finished")
        self.assertEqual(waiter.returncode, 1, stderr)         # it took the lock, found nothing to evict, judged

    def test_a_dead_holders_old_lock_is_reclaimed(self):
        dead = subprocess.Popen(["true"])
        dead.wait()
        os.makedirs(self.lock)
        with open(os.path.join(self.lock, "pid"), "w") as fh:
            fh.write("%d\n" % dead.pid)
        old = time.time() - 3 * 3600
        os.utime(self.lock, (old, old))
        orphan = os.path.join(self.fleet, "gone", "target")
        cargo_tree(orphan)
        out = self.run_preflight(free_gib=10, PLX_GC_LOCK_STALE_MIN="60")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertFalse(os.path.exists(orphan), out.stderr)
        self.assertFalse(os.path.exists(self.lock))

    # -- the inventory cache -------------------------------------------------------------------------------
    def cache_files(self):
        directory = os.path.join(self.env["PLX_BUILD_CACHE"], "disk-budget")
        return [os.path.join(directory, n) for n in sorted(os.listdir(directory))] if os.path.isdir(directory) else []

    def test_the_inventory_is_cached_for_ten_minutes_and_dropped_by_an_eviction(self):
        cargo_tree(os.path.join(self.main, "rust-modules", "target"))
        first = self.run_preflight(free_gib=10)                 # fits, nothing to evict: writes the cache
        self.assertEqual(first.returncode, 0, first.stderr)
        (cached,) = self.cache_files()
        orphan = os.path.join(self.fleet, "gone", "target")
        cargo_tree(orphan)
        second = self.run_preflight(free_gib=10)                # the cache says there is nothing
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertTrue(os.path.isdir(orphan), "re-walked the disk inside the TTL")
        old = time.time() - 11 * 60
        os.utime(cached, (old, old))
        third = self.run_preflight(free_gib=10)                 # expired: a fresh walk finds the orphan
        self.assertEqual(third.returncode, 0, third.stderr)
        self.assertFalse(os.path.exists(orphan), third.stderr)
        self.assertEqual(self.cache_files(), [], "an eviction leaves a stale inventory behind")

    def test_a_cache_that_cannot_be_read_is_ignored(self):
        cargo_tree(os.path.join(self.main, "rust-modules", "target"))
        self.run_preflight(free_gib=10)
        (cached,) = self.cache_files()
        with open(cached, "w") as fh:
            fh.write("{ not json")
        orphan = os.path.join(self.fleet, "gone", "target")
        cargo_tree(orphan)
        out = self.run_preflight(free_gib=10)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertFalse(os.path.exists(orphan), out.stderr)


# --- the pieces, as functions ---------------------------------------------------------------------------
class Pieces(unittest.TestCase):
    def test_the_inventory_survives_the_cache(self):
        tree = db.Tree(path="/work/a/rust-modules/target", kind="lane", checkout="/work/a", owner="a",
                       private_bytes=5 * GIB, alloc_bytes=5 * GIB, incr_paths=("/work/a/rust-modules/target/debug/incremental",),
                       incr_bytes=GIB, last_built=1234.5, live=db.Liveness(open_files=("rustc",), cargo_lock=True),
                       merged=True, clean=True)
        inv = db.Inventory((tree,), 7, ("/work/a/pkg",), "/cache", "origin/main", (("/work/a", "a"),))
        again = db.inventory_from_json(json.loads(json.dumps(db.inventory_to_json(inv))))
        self.assertEqual(again, inv)

    def test_a_cache_of_another_schema_is_not_trusted(self):
        data = db.inventory_to_json(db.Inventory(()))
        data["schema"] = data["schema"] + 1
        with self.assertRaises(ValueError):
            db.inventory_from_json(data)

    def test_a_tree_on_another_volume_is_judged_on_its_own_free_space_and_not_planned(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = os.path.realpath(tmp)
            args = db_args(tree=os.path.join(tmp, "elsewhere", "target"), root=tmp)
            env = {"PLX_GC_TEST_FREE_KIB": str(2 * 1024 * 1024), "PLX_FLEET_DIR": tmp, "PLX_BUILD_CACHE": tmp,
                   "PLX_GC_TMP_ROOTS": tmp, "PLX_DISK_NEED_GIB": "1"}
            with mock.patch.object(db, "device_of", side_effect=lambda p: 99 if "elsewhere" in p else 1), \
                    mock.patch.object(db, "collect", side_effect=AssertionError("planned for a tree on another volume")), \
                    contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(db.cmd_preflight(args, env), 1)
                self.assertIn("another volume", err.getvalue())
                env["PLX_GC_TEST_FREE_KIB"] = str(40 * 1024 * 1024)
                self.assertEqual(db.cmd_preflight(args, env), 0)


def db_args(tree, root, need_gib=None, wait_secs=1):
    import argparse
    return argparse.Namespace(tree=tree, root=root, need_gib=need_gib, wait_secs=wait_secs)


if __name__ == "__main__":
    unittest.main()
