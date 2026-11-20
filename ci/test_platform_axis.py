#!/usr/bin/env python3
"""The PLATFORM build axis (the Makefile's `PLATFORM`, `ci/platform_axis.py`): what it accepts, what it
implies, what it must not change, and that its script is not named after the standard library.

* `ci/platform_axis.py --list` names `webos desktop tizen`, and the Makefile reads that list the way it
  reads FLAVOR's from `ci/flavor.py --list`.
* `PLATFORM` defaults to `webos`, takes any of the three from the command line or the environment, and
  refuses anything else at parse time: an unknown name, an empty one, two words, a `%` (a `filter` of the
  value against the list would pass the last two).
* The refusal comes before the Makefile's stamp reset, whatever the goal: a mistyped `PLATFORM` beside a
  goal that builds must not delete the last build's binary. That is read from a seeded scratch tree, by
  the files surviving (a refusal below the reset would still print the same error).
* `sim`, `sim-*`, `macapp` and `macapp-zip` imply `desktop` when `PLATFORM` was not given and an explicit
  value wins. That depends on the GOALS, and `make -s print-platform sim-macos` would BUILD the simulator,
  so it is read from a dry run (`make -n`, which prints the recipe line `echo 'desktop'` instead of running
  it) in a scratch tree: a dry run beside a goal that builds is not a pure query, so its parse-time reset
  of `pkg/` (see ci/test_arm_profile.py) must not reach this checkout.
* The compatibility axis is not the platform axis: `print-rust-target` answers the same under every
  `PLATFORM`.
* Both new query goals are in the Makefile's side-effect-free set: asking resets nothing under `pkg/`. A
  control proves the seeded tree below would have been reset by a goal that builds.
* The script's name. Scripts in ci/ `import platform` and Python puts a script's own directory first on
  `sys.path`, so a `ci/platform.py` would replace the standard library module for all of them. A probe run
  from `ci/` must get the real one, each existing consumer must import it from there, and a control proves
  the probe would notice a shadow.

Pure queries run from the checkout. Everything that dry-runs a goal runs in a scratch tree that symlinks the
checkout's files but owns real `pkg/` and `vendor/` directories, and never dry-runs a goal whose recipe
re-enters make (`check`, `install`...): `$(MAKE)` lines run even under `-n`.
"""
from __future__ import annotations

import ast
import importlib.util
import os
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / "ci"
PLATFORMS = ("webos", "desktop", "tizen")
RUST_TARGET = "armv7-unknown-linux-gnueabi"
# What the Makefile reads the environment for, which a developer's shell or an outer make may have set.
SCRUBBED = ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "PLATFORM", "FLAVOR", "RELEASE", "SYMBOLS", "LAB", "CI",
            "GITHUB_ACTIONS", "ARM_PROFILE", "TV", "PLAYBACK_BACKENDS", "SIM_TDIR", "MACAPP_TDIR")
# The goals the Makefile implies `desktop` for, one per kind of recipe: the simulators, their aliases and
# their helpers, and the macOS app. Every one is dry-run below; none of them re-enters make.
DESKTOP_GOALS = ("sim", "sim-macos", "sim-linux", "sim-wsl", "sim-run", "sim-macos-run", "sim-shot",
                 "sim-macos-shot", "sim-token", "sim-macos-token", "sim-play", "sim-macos-play", "sim-clean",
                 "sim-macos-clean", "macapp", "macapp-zip")
# Goals that build the television package: they stay on the default. (Not `check` or `install`, whose
# recipes run `$(MAKE)` even under `-n`.)
WEBOS_GOALS = ("all", "ipk")
STAMP = "pkg/.build-config"
BINARY = "pkg/plxnative"
SEEDED_STAMP = "seeded by test_platform_axis"


def environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k not in SCRUBBED}
    env.update(extra or {})
    return env


def make(*args: str, cwd: Path = ROOT, dry: bool = False, env: dict[str, str] | None = None
         ) -> subprocess.CompletedProcess:
    cmd = ["make", "-n" if dry else "", "-s", "--no-print-directory", "-C", str(cwd), *args]
    return subprocess.run([c for c in cmd if c], capture_output=True, text=True, env=environment(env),
                          timeout=120)


class PlatformList(unittest.TestCase):
    def run_script(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(CI / "platform_axis.py"), *args], capture_output=True,
                              text=True, timeout=30)

    def test_the_list_names_the_three_platforms(self):
        proc = self.run_script("--list")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout, "webos desktop tizen\n")

    def test_anything_else_is_a_usage_error(self):
        for args in ((), ("--bogus",), ("--list", "extra"), ("webos",)):
            with self.subTest(args=args):
                proc = self.run_script(*args)
                self.assertEqual(proc.returncode, 2)
                self.assertNotEqual(proc.stdout.splitlines()[:1], ["webos desktop tizen"])

    def test_it_imports_nothing_but_sys(self):
        """Every `make` invocation spawns it (it is the FLAVOR cost again), and each import adds to the bill."""
        tree = ast.parse((CI / "platform_axis.py").read_text())
        imported = {alias.name for node in ast.walk(tree) if isinstance(node, ast.Import) for alias in node.names}
        imported |= {node.module for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)}
        self.assertEqual(imported, {"sys"})

    def test_the_makefile_reads_the_list_the_way_it_reads_flavors(self):
        text = (ROOT / "Makefile").read_text()
        self.assertRegex(text, r"(?m)^PLATFORMS\s*:=\s*\$\(shell python3 ci/platform_axis\.py --list\)\s*$")
        self.assertRegex(text, r"(?m)^PLATFORM\s*\?=\s*webos\s*$")


class PlatformVariable(unittest.TestCase):
    def query(self, *args: str, env: dict[str, str] | None = None) -> str:
        proc = make("print-platform", *args, env=env)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stderr, "")
        return proc.stdout.strip()

    def test_the_default_is_webos(self):
        self.assertEqual(self.query(), "webos")

    def test_every_listed_platform_is_accepted_from_the_command_line_and_the_environment(self):
        for platform in PLATFORMS:
            with self.subTest(platform=platform):
                self.assertEqual(self.query(f"PLATFORM={platform}"), platform)
                self.assertEqual(self.query(env={"PLATFORM": platform}), platform)

    def test_anything_else_is_refused_at_parse_time(self):
        for value in ("webso", "WEBOS", "", "webos bogus", "webos webos", "%", "web%", "android"):
            with self.subTest(value=value):
                proc = make("print-platform", f"PLATFORM={value}")
                self.assertNotEqual(proc.returncode, 0)
                self.assertEqual(proc.stdout, "")
                self.assertIn(f'unknown PLATFORM "{value}"', proc.stderr)
                self.assertIn("one of: webos desktop tizen", proc.stderr)

    def test_the_rust_target_is_not_chosen_by_the_platform(self):
        """The compatibility axis (Rust target, sysroot, TFM, firmware floor, signing) is separate."""
        for platform in PLATFORMS:
            with self.subTest(platform=platform):
                proc = make("print-rust-target", f"PLATFORM={platform}")
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertEqual(proc.stdout.strip(), RUST_TARGET)
        self.assertEqual(make("print-rust-target").stdout.strip(), RUST_TARGET)

    def test_the_existing_queries_do_not_move(self):
        """`make -s print-flavor print-appid` is what every tool asks, and PLATFORM changes neither."""
        for platform in PLATFORMS:
            with self.subTest(platform=platform):
                proc = make("print-flavor", "print-appid", "print-appport", f"PLATFORM={platform}")
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertEqual(proc.stdout.split(), ["debug", "com.beb.plxnative.debug", "8911"])


class ScratchTree(unittest.TestCase):
    """A scratch checkout to dry-run goals in. It holds symlinks to the checkout's own files (the Makefile
    reads `ci/` and `tools/` while it parses) and REAL `pkg/` and `vendor/` directories, because the
    parse-time reset removes paths under exactly those two and, through a symlink, would delete this
    checkout's binary and its FFmpeg header sentinel (ci/test_arm_profile.py has the whole account)."""

    def setUp(self):
        td = tempfile.TemporaryDirectory(prefix="plx-platform.")
        self.addCleanup(td.cleanup)
        self.tree = Path(td.name)
        for entry in ROOT.iterdir():
            if entry.name not in (".git", "pkg", "vendor"):
                (self.tree / entry.name).symlink_to(entry)
        (self.tree / "pkg").mkdir()
        (self.tree / "vendor").mkdir()
        # The package's tracked inputs, so `ipk` has what it names; build outputs stay out.
        (self.tree / "pkg/appinfo.json").write_text((ROOT / "pkg/appinfo.json").read_text())

    def dry(self, *args: str, env: dict[str, str] | None = None) -> list[str]:
        """The recipe lines a dry run printed. `-n` prints even an `@`-silenced line, minus the `@`."""
        proc = make(*args, cwd=self.tree, dry=True, env=env)
        self.assertEqual(proc.returncode, 0, (args, proc.stderr[-400:]))
        return proc.stdout.splitlines()

    def seed(self) -> None:
        """The tree the way a finished build leaves it: a stamp that is not the current configuration's (so
        the parse-time reset would rewrite it) and a built binary (which the reset would delete)."""
        (self.tree / STAMP).write_text(SEEDED_STAMP)
        (self.tree / BINARY).write_text("a built binary\n")

    def assert_untouched(self, what: str) -> None:
        self.assertEqual((self.tree / STAMP).read_text(), SEEDED_STAMP, f"{what} rewrote the build stamp")
        self.assertTrue((self.tree / BINARY).exists(), f"{what} deleted a built binary")


class ImpliedPlatform(ScratchTree):
    def platform_of(self, *goals: str, env: dict[str, str] | None = None) -> str:
        """The value `print-platform` would print beside these goals: its recipe line `echo '<value>'`."""
        shown = [m.group(1) for line in self.dry("print-platform", *goals, env=env)
                 if (m := re.fullmatch(r"echo '(webos|desktop|tizen)'", line))]
        self.assertEqual(len(shown), 1, (goals, shown))
        return shown[0]

    def test_the_simulators_and_the_macos_app_imply_desktop(self):
        for goal in DESKTOP_GOALS:
            with self.subTest(goal=goal):
                self.assertEqual(self.platform_of(goal), "desktop")

    def test_goals_that_build_the_television_package_stay_on_the_default(self):
        for goal in WEBOS_GOALS:
            with self.subTest(goal=goal):
                self.assertEqual(self.platform_of(goal), "webos")
        self.assertEqual(self.platform_of(), "webos")

    def test_a_goal_that_is_not_one_of_them_does_not_imply_it(self):
        self.assertEqual(self.platform_of("print-appid"), "webos")
        self.assertEqual(self.platform_of("print-appid", "ipk"), "webos")

    def test_an_explicit_value_wins_over_the_implication(self):
        self.assertEqual(self.platform_of("sim-macos", "PLATFORM=webos"), "webos")
        self.assertEqual(self.platform_of("macapp", "PLATFORM=tizen"), "tizen")
        self.assertEqual(self.platform_of("sim-macos", env={"PLATFORM": "webos"}), "webos")
        self.assertEqual(self.platform_of("all", "PLATFORM=desktop"), "desktop")

    def test_asking_never_resets_a_built_tree_and_a_building_goal_does(self):
        """Seed a tree the way a finished build leaves it, then ask the two new questions: nothing moves. The
        control (a goal that builds, beside the same query) must reset the same files, or this proves nothing."""
        for goal in ("print-platform", "print-rust-target"):
            with self.subTest(goal=goal):
                self.seed()
                for args in ((goal,), (goal, "PLATFORM=desktop"), (goal, "PLATFORM=tizen")):
                    proc = make(*args, cwd=self.tree)
                    self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assert_untouched(f"make {goal}")
        self.seed()
        self.dry("print-platform", "all")
        self.assertFalse((self.tree / BINARY).exists(), "control: a building goal must reset the seeded tree")
        self.assertNotEqual((self.tree / STAMP).read_text(), SEEDED_STAMP)

    def test_the_queries_are_registered_as_queries(self):
        text = (ROOT / "Makefile").read_text().replace("\\\n", " ")
        for goal in ("print-platform", "print-rust-target"):
            with self.subTest(goal=goal):
                self.assertRegex(text, rf"(?m)^QUERY_GOALS\s*=[^\n]*\b{goal}\b")
                self.assertRegex(text, rf"(?m)^{goal}:")


class RefusedPlatform(ScratchTree):
    def test_a_refused_value_is_refused_whatever_the_goal(self):
        """The refusal is at parse time, above the stamp reset that deletes `pkg/plxnative` and the staged
        libraries, so a mistyped value beside a goal that builds (`all`, `sim-macos`) leaves a built tree
        alone. The error text alone cannot show that (a refusal moved below the reset prints the same line
        after the reset ran), so the seeded stamp and binary must survive. The control in `ImpliedPlatform`
        shows that a building goal does reset them."""
        for goal in ("all", "sim-macos", "print-appid"):
            with self.subTest(goal=goal):
                self.seed()
                proc = make("PLATFORM=bogus", goal, cwd=self.tree, dry=True)
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn('unknown PLATFORM "bogus"', proc.stderr)
                self.assert_untouched(f"make PLATFORM=bogus {goal}")


def stdlib_names() -> frozenset[str]:
    return frozenset(getattr(sys, "stdlib_module_names", ())) | {"platform"}


def imports_platform(path: Path) -> bool:
    tree = ast.parse(path.read_text())
    return any(isinstance(node, ast.Import) and any(alias.name == "platform" for alias in node.names)
               for node in ast.walk(tree))


def consumers() -> list[Path]:
    """The scripts in ci/ that import the standard library's `platform`."""
    return sorted(p for p in CI.glob("*.py") if p != Path(__file__).resolve() and imports_platform(p))


# Imports a file the way a script run from its own directory sees its imports: that directory first on
# `sys.path`. Prints where `platform` came from and what `platform.system()` answered.
PROBE = textwrap.dedent("""\
    import importlib.util, pathlib, sys
    here = pathlib.Path(sys.argv[1])
    sys.path.insert(0, str(here))
    if len(sys.argv) > 2:
        spec = importlib.util.spec_from_file_location("consumer", sys.argv[2])
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        platform = module.platform
    else:
        import platform
    print(platform.__file__)
    print(platform.system() != "" and platform.machine() != "")
    """)


class StandardLibraryIsNotShadowed(unittest.TestCase):
    def probe(self, directory: Path, consumer: Path | None = None) -> tuple[Path, str]:
        args = [sys.executable, "-B", "-c", PROBE, str(directory)] + ([str(consumer)] if consumer else [])
        proc = subprocess.run(args, capture_output=True, text=True, timeout=60, cwd=directory)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        where, works = proc.stdout.splitlines()
        return Path(where).resolve(), works

    def test_there_are_consumers_to_protect(self):
        self.assertGreaterEqual({p.name for p in consumers()},
                                {"mkmacapp.py", "build-libass.py", "test-compat.py"})

    def test_nothing_in_ci_is_named_like_a_standard_library_module(self):
        shadows = sorted(p.name for p in CI.iterdir()
                         if (p.suffix == ".py" or (p / "__init__.py").exists()) and p.stem in stdlib_names())
        self.assertEqual(shadows, [], "a module in ci/ shadows the standard library for every script run from ci/")

    def test_a_probe_run_from_ci_gets_the_real_platform(self):
        where, works = self.probe(CI)
        self.assertNotEqual(where.parent, CI.resolve())
        self.assertEqual((where.name, works), ("platform.py", "True"))

    def test_every_existing_consumer_imports_the_real_platform_from_ci(self):
        for script in consumers():
            with self.subTest(script=script.name):
                where, works = self.probe(CI, script)
                self.assertNotEqual(where.parent, CI.resolve())
                self.assertEqual((where.name, works), ("platform.py", "True"))

    def test_the_consumers_that_parse_arguments_still_start_from_ci(self):
        for script in ("test-compat.py", "mkmacapp.py"):
            with self.subTest(script=script):
                proc = subprocess.run([sys.executable, script, "--help"], capture_output=True, text=True,
                                      timeout=60, cwd=CI)
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertIn("usage:", proc.stdout.lower())

    def test_the_probe_notices_a_shadow(self):
        """The control: with a `platform.py` beside the script the same probe must report that file, or the
        tests above would pass whatever ci/ held."""
        with tempfile.TemporaryDirectory(prefix="plx-shadow.") as td:
            fake = Path(td)
            (fake / "platform.py").write_text('def system():\n    return "shadow"\n\ndef machine():\n    return "shadow"\n')
            consumer = fake / "consumer.py"
            consumer.write_text("import platform\n")
            for use in (None, consumer):
                with self.subTest(consumer=bool(use)):
                    where, _ = self.probe(fake, use)
                    self.assertEqual(where, (fake / "platform.py").resolve())


if __name__ == "__main__":
    unittest.main()
