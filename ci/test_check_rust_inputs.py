#!/usr/bin/env python3
"""ci/check-rust-inputs.py's self-test, on synthetic trees in a private temp dir.

The gate's one job is to see a crate `make` would not: so the cases plant one (a path dependency of
the app, then of a layer crate, then a build-dependency) and demand a failure that names it, then
check the real repository is green and that check-python-steps.txt still runs the gate.
"""
from pathlib import Path
import importlib.util
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("check_rust_inputs", ROOT / "ci" / "check-rust-inputs.py")
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)


def tree(base: Path, listed: list[str], app_deps: dict[str, str], crates: dict[str, str]) -> Path:
    """A checkout with a Makefile whose RUST_INPUTS searches `listed`, an app manifest whose
    [dependencies] are `app_deps` (name -> path), and crates (dir -> manifest text)."""
    (base / "rust-modules" / "src").mkdir(parents=True)
    (base / "Makefile").write_text(
        "RUST_INPUTS := $(shell find " + " ".join(listed) + " -type f 2>/dev/null)\n"
    )
    deps = "".join(f'{n} = {{ path = "{p}" }}\n' for n, p in app_deps.items())
    (base / "rust-modules" / "Cargo.toml").write_text(f'[package]\nname = "app"\n[dependencies]\n{deps}')
    for d, manifest in crates.items():
        (base / "rust-modules" / d / "src").mkdir(parents=True)
        (base / "rust-modules" / d / "Cargo.toml").write_text(manifest)
    return base


def leaf(name: str) -> str:
    return f'[package]\nname = "{name}"\n'


class Gate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_tree_whose_list_covers_every_crate_is_green(self):
        root = tree(self.base, ["rust-modules/src", "rust-modules/base"], {"b": "base"}, {"base": leaf("b")})
        self.assertEqual(gate.failures(root), [])

    def test_an_app_dependency_missing_from_the_list_fails_and_is_named(self):
        root = tree(
            self.base,
            ["rust-modules/src", "rust-modules/base"],
            {"b": "base", "j": "jellyfin"},
            {"base": leaf("b"), "jellyfin": leaf("j")},
        )
        bad = gate.failures(root)
        self.assertEqual(len(bad), 1)
        self.assertIn("rust-modules/jellyfin/", bad[0])

    def test_a_dependency_of_a_layer_crate_is_followed(self):
        layer = '[package]\nname = "b"\n[dependencies]\ndeep = { path = "../deep" }\n'
        root = tree(
            self.base,
            ["rust-modules/src", "rust-modules/base"],
            {"b": "base"},
            {"base": layer, "deep": leaf("deep")},
        )
        self.assertIn("rust-modules/deep/", " ".join(gate.failures(root)))

    def test_a_build_dependency_counts_and_a_dev_dependency_does_not(self):
        layer = (
            '[package]\nname = "b"\n[build-dependencies]\nbs = { path = "../build_support" }\n'
            '[dev-dependencies]\nfixture = { path = "../fixture" }\n'
        )
        root = tree(
            self.base,
            ["rust-modules/src", "rust-modules/base"],
            {"b": "base"},
            {"base": layer, "build_support": leaf("bs"), "fixture": leaf("fixture")},
        )
        bad = " ".join(gate.failures(root))
        self.assertIn("rust-modules/build_support/", bad)
        self.assertNotIn("fixture", bad)

    def test_a_listed_directory_that_does_not_exist_fails(self):
        root = tree(self.base, ["rust-modules/src", "rust-modules/gone"], {}, {})
        self.assertIn("rust-modules/gone/, which does not exist", " ".join(gate.failures(root)))

    def test_the_repository_is_green(self):
        self.assertEqual(gate.failures(ROOT), [])

    def test_check_python_steps_runs_the_gate(self):
        steps = (ROOT / "ci" / "check-python-steps.txt").read_text().splitlines()
        self.assertIn("python3 ci/check-rust-inputs.py", steps)


if __name__ == "__main__":
    unittest.main()
