#!/usr/bin/env python3
"""Fail if building the storage helper compiles the app library.

`plxnative-storage` is the small LS2 service binary that holds the private store; it shares a few
source files with the app (`src/storage_service/*.rs`, `src/storage/state.rs`) but never links the
app crate. While it was a `[[bin]]` of the `plxnative-modules` package, cargo nevertheless built
the whole ~423k-line library for it first (a bin of a package depends on that package's lib), so
`make check` paid two extra host library builds and the `pkg/plxnative-storage` rule paid a third,
ARM one, for a binary that used none of it. The helper is its own workspace package now, and this
holds that line.

It asks cargo for the resolved UNIT GRAPH of BOTH invocations the repo uses for the helper
(`cargo ... --unit-graph`, nightly `-Zunstable-options`) instead of compiling them:
  * `cargo test <bin> --no-run`, what `make check` builds before running the helper's unit tests;
  * `cargo rustc <bin> --no-default-features`, the shape of the `pkg/plxnative-storage` rule. That
    rule passes `--release --target arm-unknown-linux-gnueabi`; here it is the HOST, dev-profile
    graph of the same package and binary, because the property checked (which crates cargo would
    compile for this binary) is decided by the package graph, not by the profile or the triple.
and fails on any unit that belongs to the app package -- its library, and also its build script,
which is a compile of the app package too. The graph is what cargo would build, fresh or not, so a
cached library still counts, and no target directory is read, so stale files from an older
checkout cannot matter. This used to compile both invocations and read the `compiler-artifact`
records (about 7 s of rustc on the serial cargo path of `make check`); the unit graph says the same
thing without running rustc.

The package that owns the binary is asked of `cargo metadata` rather than hard-coded, so the same
script is red on the old layout (the owner IS `plxnative-modules`) for the right reason.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parent.parent
APP_CRATE = "plxnative_modules"
APP_PACKAGE = "plxnative-modules"
BIN = "plxnative-storage"
NIGHTLY = os.environ.get("RUST_NIGHTLY", "nightly")


def app_package_units(graph):
    """Names of the units in a `--unit-graph` document that belong to the app package.

    Matches on the unit's package id (the library AND its build script) as well as on the library
    target's name, so a unit cannot hide behind either spelling.
    """
    found = []
    for unit in graph.get("units", []):
        target = unit.get("target") or {}
        pkg = unit.get("pkg_id") or ""
        if target.get("name") == APP_CRATE or f"#{APP_PACKAGE}@" in pkg or pkg.startswith(APP_PACKAGE + " "):
            found.append((target.get("name"), "/".join(target.get("kind") or [])))
    return found


def cargo(*args, cwd=None):
    env = dict(os.environ)
    env["PATH"] = str(Path.home() / ".cargo/bin") + os.pathsep + env.get("PATH", "")
    # cwd matters: cargo finds rust-modules/.cargo/config.toml (build-std and the codegen flags)
    # from the working directory.
    return subprocess.run(["cargo", f"+{NIGHTLY}", *args], cwd=cwd or ROOT / "rust-modules",
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)


def metadata():
    proc = cargo("metadata", "--format-version", "1", "--no-deps")
    assert proc.returncode == 0, proc.stderr[-2000:]
    return json.loads(proc.stdout)


def unit_graph(*args, cwd=None):
    """The resolved unit graph of a cargo build invocation, without compiling anything."""
    proc = cargo(*args, "--unit-graph", "-Zunstable-options", cwd=cwd)
    assert proc.returncode == 0, proc.stderr[-2000:]
    return json.loads(proc.stdout)


def owner_package():
    for package in metadata()["packages"]:
        for target in package["targets"]:
            if target["name"] == BIN and "bin" in target["kind"]:
                return package
    raise AssertionError(f"no cargo package declares a {BIN} binary")


class ParserTests(unittest.TestCase):
    PID = "path+file:///r/rust-modules#plxnative-modules@0.7.0"

    def unit(self, name, pkg_id, kind=("lib",)):
        return {"pkg_id": pkg_id, "target": {"name": name, "kind": list(kind)}}

    def test_flags_the_app_library(self):
        graph = {"units": [self.unit(APP_CRATE, self.PID)]}
        self.assertEqual(app_package_units(graph), [(APP_CRATE, "lib")])

    def test_flags_the_app_build_script(self):
        graph = {"units": [self.unit("build-script-build", self.PID, ("custom-build",))]}
        self.assertEqual(app_package_units(graph), [("build-script-build", "custom-build")])

    def test_flags_the_old_package_id_spelling(self):
        graph = {"units": [self.unit("anything", f"{APP_PACKAGE} 0.7.0 (path+file:///r/rust-modules)")]}
        self.assertEqual(len(app_package_units(graph)), 1)

    def test_other_crates_and_the_helper_itself_are_fine(self):
        graph = {"units": [self.unit("serde_json", "registry+https://x#serde_json@1.0.0"),
                           self.unit(BIN, "path+file:///r/rust-modules/storage#plxnative-storage@0.0.0",
                                     ("bin",))]}
        self.assertEqual(app_package_units(graph), [])


class StoragePackageTests(unittest.TestCase):
    def test_binary_is_not_in_the_app_package(self):
        package = owner_package()
        self.assertNotEqual(package["name"], APP_PACKAGE,
                            f"{BIN} is a bin of {APP_PACKAGE}, so cargo builds the app library for it")
        self.assertNotIn(APP_PACKAGE, [d["name"] for d in package["dependencies"]])

    def assert_no_app_library(self, label, *args):
        package = owner_package()["name"]
        graph = unit_graph(args[0], "-p", package, *args[1:])
        self.assertTrue(any(u["target"]["name"] == BIN for u in graph["units"]),
                        f"{label}: the unit graph does not contain {BIN} at all")
        self.assertEqual(app_package_units(graph), [], f"{label}: building {BIN} would compile {APP_CRATE}")

    def test_host_unit_test_build_compiles_no_app_library(self):
        self.assert_no_app_library("cargo test", "test", "--bin", BIN, "--no-run")

    def test_release_shaped_build_compiles_no_app_library(self):
        self.assert_no_app_library("cargo rustc", "rustc", "--bin", BIN, "--no-default-features")


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
