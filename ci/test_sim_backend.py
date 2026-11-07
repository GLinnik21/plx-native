#!/usr/bin/env python3
"""Every simulator build that drops the default features still selects the Feed backend.

`hostsim` swaps the television's native video sink for the simulator's (`plx_play_feed::ffi_host`),
but that stand-in is the Feed backend's, so it exists only when `play-feed` is built. A simulator
built `--no-default-features --features hostsim` selects NO backend: its app installs `NoSink`, so
Play can only ever reach the failure read-out and the clock sink (`plxnative-clocksink`) has nothing
to arm. Three builds drift into that state because they drop `devtools` and `devtriggers` with
`--no-default-features` and then list `hostsim` alone: the `.app` bundle (`ci/mkmacapp.py`), the
screenshot simulator (`make screenshots-sim`) and the demo-video simulator (`make site-video-sim`);
so does the CI step that type-checks the simulator feature set, which then proves nothing about the
configuration the other three ship.

The scan finds every cargo command line in the files that can hold one, joins its continuations and
`--features` flags, and fails the ones that name `hostsim`, pass `--no-default-features` and lack
`play-feed`. The named sites below are asserted to be FOUND, so a rewrite that moves a line out of
the scan's reach cannot turn the rule into a vacuous pass.
"""
import ast
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

MAKEFILE = "Makefile"
MKMACAPP = "ci/mkmacapp.py"
CI_YML = ".github/workflows/ci.yml"


def feature_flags(tokens):
    """`(no_default_features, features)` of one cargo command, from its words."""
    no_default, features = False, set()
    it = iter(tokens)
    for tok in it:
        if tok == "--no-default-features":
            no_default = True
        elif tok == "--features":
            features.update(next(it, "").split(","))
        elif tok.startswith("--features="):
            features.update(tok.split("=", 1)[1].split(","))
    features.discard("")
    return no_default, features


def logical_lines(text):
    """Source lines with backslash continuations joined, as `(first line number, text)`."""
    out, buf, start = [], "", 0
    for n, line in enumerate(text.splitlines(), 1):
        if not buf:
            start = n
        if line.rstrip().endswith("\\"):
            buf += line.rstrip()[:-1] + " "
            continue
        out.append((start, buf + line))
        buf = ""
    if buf:
        out.append((start, buf))
    return out


def text_cargo_commands(path):
    """Every `cargo ... --features ...` in a Makefile or workflow, with its flags."""
    found = []
    for n, line in logical_lines((ROOT / path).read_text()):
        if not re.search(r"(^|[\s@\"'])cargo\b", line):
            continue
        # `$(if $(LAB), --features lab-diagnostics,)` and the like: the flag words are what count.
        tokens = [t.rstrip(",)") for t in re.split(r"\s+", line.replace("$(if $(LAB),", " "))]
        found.append((path, n, line.strip(), *feature_flags(tokens)))
    return found


def python_cargo_commands(path):
    """Every `run("cargo", ...)` / `["cargo", ...]` in a Python file, from its string constants."""
    found = []
    tree = ast.parse((ROOT / path).read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            head = node.args[0] if node.args else None
        elif isinstance(node, ast.List):
            head = node.elts[0] if node.elts else None
        else:
            continue
        if not (isinstance(head, ast.Constant) and head.value == "cargo"):
            continue
        tokens = [c.value for c in ast.walk(node) if isinstance(c, ast.Constant) and isinstance(c.value, str)]
        found.append((path, node.lineno, " ".join(tokens), *feature_flags(tokens)))
    return found


def scan():
    commands = []
    for path in (MAKEFILE, CI_YML):
        commands += text_cargo_commands(path)
    for path in sorted(p.relative_to(ROOT).as_posix() for p in (ROOT / "ci").glob("*.py")):
        commands += python_cargo_commands(path)
    return commands


def sim_commands(commands):
    return [c for c in commands if "hostsim" in c[4]]


class SimulatorBackend(unittest.TestCase):
    def test_the_scan_reaches_the_builds_it_exists_for(self):
        sims = sim_commands(scan())
        reached = {(path, bool(no_default)) for path, _, _, no_default, _ in sims}
        for site in (MKMACAPP, MAKEFILE, CI_YML):
            with self.subTest(site):
                self.assertIn((site, True), reached, f"no `--no-default-features --features hostsim` build found in {site}")
        makefile_lines = " ".join(line for path, _, line, _, _ in sims if path == MAKEFILE)
        for target_flag in ("--bin plxnative-sim", "placeholder-sentinel", "devtriggers"):
            self.assertIn(target_flag, makefile_lines)

    def test_a_simulator_without_default_features_selects_the_feed_backend(self):
        bad = [
            f"{path}:{n}: {line}"
            for path, n, line, no_default, features in sim_commands(scan())
            if no_default and "play-feed" not in features
        ]
        self.assertEqual(
            bad, [],
            "a simulator built --no-default-features selects no backend, so it installs NoSink and "
            "can never play: add play-feed to its --features",
        )

    def test_each_scripted_simulator_build_is_pinned_to_its_features(self):
        """The three bundle/screenshot/video builds and the CI type-check, spelled out."""
        pinned = {
            MKMACAPP: {"hostsim", "play-feed"},
            CI_YML: {"hostsim", "play-feed"},
        }
        sims = sim_commands(scan())
        for site, expected in pinned.items():
            with self.subTest(site):
                no_default_builds = [f for path, _, _, nd, f in sims if path == site and nd]
                self.assertTrue(no_default_builds)
                for features in no_default_builds:
                    self.assertTrue(expected <= features, f"{site}: {sorted(features)} lacks {sorted(expected - features)}")
        by_target = {}
        target = None
        for line in (ROOT / MAKEFILE).read_text().splitlines():
            m = re.match(r"^([A-Za-z0-9_.-]+):(?!=)", line)
            if m:
                target = m.group(1)
            if target in ("screenshots-sim", "site-video-sim") and "hostsim" in line:
                by_target.setdefault(target, set()).update(feature_flags(re.split(r"\s+", line))[1])
        for target in ("screenshots-sim", "site-video-sim"):
            with self.subTest(target):
                self.assertIn("play-feed", by_target.get(target, set()), f"make {target} lacks play-feed")
                self.assertIn("hostsim", by_target[target])


if __name__ == "__main__":
    unittest.main()
