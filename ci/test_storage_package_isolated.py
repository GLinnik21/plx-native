#!/usr/bin/env python3
"""Fail if building the storage helper compiles the app library.

`plxnative-storage` is the small LS2 service binary that holds the private store; it shares a few
source files with the app (`src/storage_service/*.rs`, `src/storage/state.rs`) but never links the
app crate. While it was a `[[bin]]` of the `plxnative-modules` package, cargo nevertheless built
the whole ~423k-line library for it first (a bin of a package depends on that package's lib), so
`make check` paid two extra host library builds and the `pkg/plxnative-storage` rule paid a third,
ARM one, for a binary that used none of it. The helper is its own workspace package now, and this
holds that line.

It runs BOTH invocations the repo uses for the helper:
  * `cargo test <bin> --no-run`, what `make check` builds before running the helper's unit tests;
  * `cargo rustc <bin> --no-default-features`, the shape of the `pkg/plxnative-storage` rule. That
    rule passes `--release --target arm-unknown-linux-gnueabi`; here it is the HOST, dev-profile
    build of the same package and binary, because a 32-bit ARM `-Z build-std` build is far too
    heavy for a unit gate and the property checked (which crates cargo compiles for this binary)
    is decided by the package graph, not by the profile or the triple.
and reads only cargo's own `compiler-artifact` records, failing on any whose target is
`plxnative_modules` -- a `"fresh": true` record counts, because a cached library is still a library
the helper was made to depend on. It never lists a target directory, so stale files from an older
checkout cannot matter; cargo's stdout also carries the test harness's plain text, so only lines
that parse as a JSON object are looked at.

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


def app_library_artifacts(stdout):
    """Names of the `compiler-artifact` records in cargo's JSON stream that are the app library."""
    offences = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if not isinstance(record, dict) or record.get("reason") != "compiler-artifact":
            continue
        if (record.get("target") or {}).get("name") == APP_CRATE:
            offences.append((record["target"]["name"], record.get("fresh")))
    return offences


def cargo(*args):
    env = dict(os.environ)
    env["PATH"] = str(Path.home() / ".cargo/bin") + os.pathsep + env.get("PATH", "")
    # cwd matters: cargo finds rust-modules/.cargo/config.toml (the codegen flags) from the working
    # directory, and a different flag set is a different fingerprint, i.e. a full rebuild.
    return subprocess.run(["cargo", f"+{NIGHTLY}", *args], cwd=ROOT / "rust-modules",
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)


def metadata():
    proc = cargo("metadata", "--format-version", "1", "--no-deps")
    assert proc.returncode == 0, proc.stderr[-2000:]
    return json.loads(proc.stdout)


def owner_package():
    for package in metadata()["packages"]:
        for target in package["targets"]:
            if target["name"] == BIN and "bin" in target["kind"]:
                return package
    raise AssertionError(f"no cargo package declares a {BIN} binary")


class ParserTests(unittest.TestCase):
    def record(self, **over):
        rec = {"reason": "compiler-artifact", "target": {"name": APP_CRATE, "crate_types": ["rlib"]},
               "filenames": ["/t/libplxnative_modules-abc.rlib"], "fresh": True}
        rec.update(over)
        return json.dumps(rec)

    def test_ignores_harness_text_and_other_reasons(self):
        out = "\n".join(["running 0 tests", "{not json", json.dumps({"reason": "build-finished"}),
                         json.dumps({"reason": "build-script-executed", "package_id": APP_CRATE})])
        self.assertEqual(app_library_artifacts(out), [])

    def test_flags_the_app_library_even_when_fresh(self):
        self.assertEqual(app_library_artifacts(self.record(fresh=True)), [(APP_CRATE, True)])

    def test_other_crates_are_fine(self):
        out = self.record(target={"name": "serde_json", "crate_types": ["rlib"]})
        self.assertEqual(app_library_artifacts(out), [])


class StoragePackageTests(unittest.TestCase):
    def test_binary_is_not_in_the_app_package(self):
        package = owner_package()
        self.assertNotEqual(package["name"], APP_PACKAGE,
                            f"{BIN} is a bin of {APP_PACKAGE}, so cargo builds the app library for it")
        self.assertNotIn(APP_PACKAGE, [d["name"] for d in package["dependencies"]])

    def assert_no_app_library(self, label, *args):
        package = owner_package()["name"]
        proc = cargo(args[0], "-p", package, *args[1:], "--message-format=json")
        self.assertEqual(proc.returncode, 0, proc.stderr[-2000:])
        self.assertEqual(app_library_artifacts(proc.stdout), [],
                         f"{label}: building {BIN} compiled {APP_CRATE}")

    def test_host_unit_test_build_compiles_no_app_library(self):
        self.assert_no_app_library("cargo test", "test", "--bin", BIN, "--no-run")

    def test_release_shaped_build_compiles_no_app_library(self):
        self.assert_no_app_library("cargo rustc", "rustc", "--bin", BIN, "--no-default-features")


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
