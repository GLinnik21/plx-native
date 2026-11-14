#!/usr/bin/env python3
"""`make test-crate C=…` and tools/test-crate.py: the crate-only loop must run the SAME tests.

The property that silently breaks is the feature set. The application crate's default features turn
on the dev surface in every layer crate, and cargo unifies features only across the packages it is
asked to build, so a bare `-p plx_plex` compiles a different crate (458 tests instead of 466) that
shares no artifact with `make test-fast`. These tests pin that the command carries the full
suite's per-crate features, that `DEPS=1` and the name aliases resolve from `cargo metadata`
rather than a list, and that the Makefile's recipe is wired to `test-fast`'s tree and refusals.
Hermetic: a fake `cargo` answers `metadata`, `--unit-graph` and `--no-run`; nothing is compiled.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "tools" / "test-crate.py"
SUITE = "plxnative-modules plx_base plx_machine plx_ui plx_screens"

# plx_base <- plx_machine <- plx_ui <- plx_screens <- app; plx_ui also names a third-party crate.
WORKSPACE = {
    "plxnative-modules": ["plx_base", "plx_machine", "plx_ui", "plx_screens"],
    "plx_base": [],
    "plx_machine": ["plx_base"],
    "plx_ui": ["plx_base", "plx_machine", "image"],
    "plx_screens": ["plx_base", "plx_ui"],
}
# What the FULL build enabled, per package (a union over its units).
FULL_FEATURES = {
    "plxnative-modules": ["default", "devtriggers"],
    "plx_base": ["devtriggers", "test-support", "threadcheck"],
    "plx_machine": ["devtriggers", "test-support"],
    "plx_ui": ["devtriggers", "threadcheck"],
    "plx_screens": ["devtriggers"],
    "image": ["png"],
    "serde": ["derive"],
}

FAKE_CARGO = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
data = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "world.json")))
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "calls.log"), "a") as f:
    f.write("incremental=%s target_dir=%s :: %s\\n" % (os.environ.get("CARGO_INCREMENTAL"),
            os.environ.get("CARGO_TARGET_DIR"), " ".join(args)))
if args[:1] and args[0].startswith("+"):
    args = args[1:]
if args[0] == "metadata":
    print(json.dumps({"packages": [{"name": n, "dependencies": [{"name": d} for d in deps]}
                                   for n, deps in data["workspace"].items()]}))
elif "--unit-graph" in args:
    # Stable cargo refuses -Z; RUSTC_BOOTSTRAP=1 is what lets the query through (the 2026-10 regression).
    if "-Z" in args and os.environ.get("RUSTC_BOOTSTRAP") != "1":
        sys.stderr.write("error: the `-Z` flag is only accepted on the nightly channel of Cargo\\n")
        sys.exit(101)
    units = [{"pkg_id": "path+file:///r/%s#%s@0.0.0" % (n, n), "features": f, "dependencies": [],
              "target": {"kind": ["lib"]}, "mode": "test"} for n, f in data["features"].items()]
    print(json.dumps({"version": 1, "units": units, "roots": []}))
elif "--no-run" in args:
    home = os.path.dirname(os.path.abspath(__file__))
    for i, a in enumerate(args):
        if a == "-p":
            n = args[i + 1]
            exe = os.path.join(home, "bin-" + n)
            open(exe, "w").write("#!/bin/sh\\necho \\"ran %s: $*\\" >> %s/bins.log\\n"
                                 "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'\\n" % (n, home))
            os.chmod(exe, 0o755)
            print(json.dumps({"reason": "compiler-artifact", "package_id": "path+file:///r/%s#%s@0.0.0" % (n, n),
                              "manifest_path": home + "/Cargo.toml", "target": {"name": n},
                              "profile": {"test": True}, "executable": exe}))
"""


class World:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        (self.dir / "world.json").write_text(json.dumps({"workspace": WORKSPACE, "features": FULL_FEATURES}))
        (self.dir / "cargo").write_text(FAKE_CARGO)
        (self.dir / "cargo").chmod(0o755)
        return self

    def __exit__(self, *exc):
        self.tmp.cleanup()

    def dry(self, *args):
        return subprocess.run([sys.executable, str(TOOL), "--suite", SUITE, "--cargo", str(self.dir / "cargo"),
                               "--dry-run", *args], capture_output=True, text=True)

    def command(self, *args):
        out = self.dry(*args)
        assert out.returncode == 0, out.stderr
        return out.stdout.split(" -- ", 1)[1].strip()


def features_of(command: str) -> set[str]:
    m = re.search(r"--features (\S+)", command)
    return set(m.group(1).split(",")) if m else set()


def packages_of(command: str) -> list[str]:
    return re.findall(r"-p (\S+)", command)


class CommandShape(unittest.TestCase):
    def test_the_crate_alone_with_the_full_builds_features(self):
        with World() as w:
            cmd = w.command("ui")
            self.assertEqual(packages_of(cmd), ["plx_ui"])
            self.assertIn("test --lib", cmd)
            feats = features_of(cmd)
            # Its own, its direct dependencies' (workspace and third party), and nothing else.
            self.assertEqual(feats, {"plx_ui/devtriggers", "plx_ui/threadcheck", "plx_base/devtriggers",
                                     "plx_base/test-support", "plx_base/threadcheck", "plx_machine/devtriggers",
                                     "plx_machine/test-support", "image/png"})
            self.assertNotIn("serde/derive", feats)

    def test_unit_graph_query_works_on_stable_and_the_bootstrap_does_not_leak(self):
        with World() as w:
            w.command("ui")  # the fake refuses -Z without RUSTC_BOOTSTRAP=1, like stable cargo
            out = subprocess.run([sys.executable, str(TOOL), "--suite", SUITE, "--cargo", str(w.dir / "cargo"),
                                  "--dry-run", "ui"], capture_output=True, text=True,
                                 env={k: v for k, v in os.environ.items() if k != "RUSTC_BOOTSTRAP"})
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertNotIn("RUSTC_BOOTSTRAP", out.stdout)

    def test_names_resolve_with_or_without_the_prefix_and_the_app_crate_is_app(self):
        with World() as w:
            for name in ("ui", "plx_ui"):
                self.assertEqual(packages_of(w.command(name)), ["plx_ui"])
            for name in ("app", "plxnative-modules", "plxnative_modules"):
                cmd = w.command(name)
                self.assertEqual(packages_of(cmd), ["plxnative-modules"])
                self.assertNotIn("--features", cmd, "the app crate resolves its own default features")

    def test_an_unknown_crate_is_refused_and_lists_the_suite(self):
        with World() as w:
            out = w.dry("nope")
            self.assertNotEqual(out.returncode, 0)
            self.assertIn("plx_base", out.stderr)

    def test_deps_adds_the_direct_dependents_from_cargo_metadata(self):
        with World() as w:
            self.assertEqual(sorted(packages_of(w.command("--deps", "machine"))),
                             ["plx_machine", "plx_ui", "plxnative-modules"])
            self.assertEqual(sorted(packages_of(w.command("--deps", "screens"))),
                             ["plx_screens", "plxnative-modules"])

    def test_the_filter_goes_to_the_runner(self):
        with World() as w:
            out = w.dry("--filter", "route::", "base")
            self.assertIn("cargo-test-parallel.py --filter route:: --", out.stdout)

    def test_several_crates_in_one_call(self):
        with World() as w:
            self.assertEqual(packages_of(w.command("base", "machine")), ["plx_base", "plx_machine"])


class MakeWiring(unittest.TestCase):
    def make(self, *args, env=None):
        full = dict(os.environ)
        for k in ("MAKEFLAGS", "MFLAGS"):
            full.pop(k, None)
        # The recipes are under test; the disk preflight has its own test (ci/test_disk_preflight.py), and a real
        # `make` would otherwise run it against this machine's disk and, below the comfort line, evict its trees.
        full["PLX_DISK_BUDGET_DONE"] = "1"
        full.update(env or {})
        return subprocess.run(["make", *args], cwd=ROOT, env=full, capture_output=True, text=True)

    def test_no_crate_named_is_a_usage_error_not_a_full_suite_run(self):
        out = self.make("test-crate")
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("name the crate", out.stderr)

    def test_release_is_refused(self):
        out = self.make("test-crate", "C=ui", "RELEASE=1")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("refused", out.stderr)

    def test_runs_in_test_fasts_tree_with_its_flags(self):
        # `make -n` prints the recipe without running it; the environment words are what share the cache.
        out = self.make("-n", "test-crate", "C=ui", "T=route::", "DEPS=1").stdout
        self.assertIn("CARGO_INCREMENTAL=1", out)
        self.assertIn("CARGO_TARGET_DIR=target-fast", out)
        self.assertIn("PLXNATIVE_RUNTIME_DIR=", out)
        self.assertIn("--deps", out)
        self.assertIn("--filter 'route::'", out)
        fast = self.make("-n", "test-fast").stdout
        for word in ("CARGO_INCREMENTAL=1", "CARGO_TARGET_DIR=target-fast"):
            self.assertIn(word, fast)

    def test_the_suite_variable_is_the_recipes_package_list(self):
        text = (ROOT / "Makefile").read_text()
        suite = re.search(r"^UNIT_SUITE\s*=\s*(.+)$", text, flags=re.M).group(1).split()
        flags = " ".join(f"-p {p}" for p in suite)
        for target in ("check-cargo-unit-default", "test-fast"):
            start = text.index(f"\n{target}:")
            recipe = text[start:start + 6000].split("\n\n")[0]
            self.assertIn(f"test --lib {flags}", recipe, target)

    def test_the_hostsim_pass_is_exactly_the_crates_that_declare_hostsim(self):
        """The `hostsim` pass runs HOSTSIM_SUITE, a subset of UNIT_SUITE (`make check`'s first run of
        every binary is the default pass). A crate left out has an identical test set in both passes:
        no `hostsim` feature in its manifest and no `feature = "hostsim"` in its source. What the guard
        does not check is the dependency side: the old pass linked those crates' test binaries
        against hostsim-built `plx_base`, `plx_gfx`, `plx_platform`, `plx_media` and `plx_play_feed`
        (`ENV_STEERABLE` paths, glyph-cache sizes, `ffi_host` present), and none of their tests
        observes those differences. A crate that gains the feature or a cfg fails here until it is
        added to HOSTSIM_SUITE (and the recipe's `-p` list)."""
        text = (ROOT / "Makefile").read_text()
        unit = re.search(r"^UNIT_SUITE\s*=\s*(.+)$", text, flags=re.M).group(1).split()
        hostsim = re.search(r"^HOSTSIM_SUITE\s*=\s*(.+)$", text, flags=re.M).group(1).split()
        self.assertEqual(hostsim, [p for p in unit if p in hostsim], "HOSTSIM_SUITE is UNIT_SUITE's order")
        self.assertTrue(set(hostsim) <= set(unit))
        start = text.index("\ncheck-cargo-unit-hostsim:")
        recipe = text[start:start + 6000].split("\n\n")[0]
        self.assertIn("test --lib " + " ".join(f"-p {p}" for p in hostsim) + " --features hostsim", recipe)
        manifests = {"plxnative-modules": ROOT / "rust-modules"}
        for member in (ROOT / "rust-modules").iterdir():
            manifest = member / "Cargo.toml"
            if manifest.is_file():
                name = re.search(r'^name\s*=\s*"([^"]+)"', manifest.read_text(), flags=re.M)
                if name:
                    manifests[name.group(1)] = member
        for crate in unit:
            root = manifests[crate]
            declares = re.search(r"^hostsim\s*=", (root / "Cargo.toml").read_text(), flags=re.M) is not None
            sources = (root / "src").rglob("*.rs")
            cfgs = [str(f.relative_to(ROOT)) for f in sources if 'feature = "hostsim"' in f.read_text(errors="replace")]
            if crate in hostsim:
                self.assertTrue(declares, f"{crate} is in HOSTSIM_SUITE but declares no `hostsim` feature, which `--features hostsim -p {crate}` needs")
            else:
                self.assertFalse(declares, f"{crate} declares `hostsim`: add it to HOSTSIM_SUITE and the recipe")
                self.assertEqual(cfgs, [], f"{crate} has a hostsim cfg: add it to HOSTSIM_SUITE and the recipe")

    def test_check_never_names_test_crate(self):
        text = (ROOT / "Makefile").read_text()
        for target in ("check-cargo-unit-default", "check-cargo-unit-hostsim"):
            start = text.index(f"\n{target}:")
            recipe = text[start:start + 6000].split("\n\n")[0]
            self.assertNotIn("test-crate", recipe)
            self.assertNotIn("target-fast", recipe)


if __name__ == "__main__":
    unittest.main()
