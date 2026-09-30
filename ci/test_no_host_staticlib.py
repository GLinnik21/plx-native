#!/usr/bin/env python3
"""Fail if a host cargo build of the app crate still emits a staticlib.

`rust-modules/Cargo.toml` declares `crate-type = ["rlib"]`: the simulator bin and the host tests
only need the rlib, and the ARM archive `make` links is produced separately by
`cargo rustc --crate-type staticlib`. A `staticlib` crate type on the crate makes EVERY cargo
build of it (`cargo build`, the simulator, anything that depends on the library) also write a
~200 MB archive that bundles every upstream crate, and makes the ARM build write an rlib nobody
links.

This reads cargo's own `compiler-artifact` records and never lists a target directory, so stale
files from an older checkout cannot matter and a `"fresh": true` record still counts. cargo's
stdout also carries the test harness's plain text, so only lines that parse as a JSON object
with reason == "compiler-artifact" are looked at.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parent.parent
CRATE = "plxnative_modules"
NIGHTLY = os.environ.get("RUST_NIGHTLY", "nightly")
# `cargo test --lib` compiles the crate only as a test harness and never asks for its library
# crate types, so it cannot show the offence. A plain `cargo build --lib` builds the library unit
# itself, the same unit the simulator bin (and any other dependent) links, and writes whatever
# `[lib] crate-type` declares. This used to ride on `cargo test --bin plxnative-storage --no-run`,
# which built the library as a dependency of the storage helper; the helper is its own package now
# and depends on no part of this crate, so the library is built directly.
CONFIGS = (("lib unit", ["build", "--lib"]),)


def staticlib_artifacts(stdout):
    """Return (filename-or-crate-type, detail) offences among the crate's compiler-artifact records."""
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
        target = record.get("target") or {}
        if target.get("name") != CRATE:
            continue
        if "staticlib" in (target.get("crate_types") or []):
            offences.append(("crate_types", ",".join(target["crate_types"])))
        for name in record.get("filenames") or []:
            if name.endswith(".a"):
                offences.append(("filename", name))
    return offences


class ParserTests(unittest.TestCase):
    def record(self, **over):
        rec = {"reason": "compiler-artifact", "target": {"name": CRATE, "crate_types": ["rlib"]},
               "filenames": ["/t/libplxnative_modules-abc.rlib"], "fresh": True}
        rec.update(over)
        return json.dumps(rec)

    def test_ignores_harness_text_and_other_reasons(self):
        out = "\n".join(["running 0 tests", "{not json", json.dumps({"reason": "build-finished"}),
                         self.record()])
        self.assertEqual(staticlib_artifacts(out), [])

    def test_flags_archive_even_when_fresh(self):
        out = self.record(filenames=["/t/libplxnative_modules.a"], fresh=True)
        self.assertEqual(staticlib_artifacts(out), [("filename", "/t/libplxnative_modules.a")])

    def test_flags_staticlib_crate_type(self):
        out = self.record(target={"name": CRATE, "crate_types": ["staticlib", "rlib"]})
        self.assertEqual(staticlib_artifacts(out), [("crate_types", "staticlib,rlib")])

    def test_other_crates_may_be_archives(self):
        out = self.record(target={"name": "zstd_sys", "crate_types": ["staticlib"]},
                          filenames=["/t/libz.a"])
        self.assertEqual(staticlib_artifacts(out), [])


class HostBuildTests(unittest.TestCase):
    def test_host_builds_emit_no_staticlib(self):
        env = dict(os.environ)
        env["PATH"] = str(Path.home() / ".cargo/bin") + os.pathsep + env.get("PATH", "")
        for label, args in CONFIGS:
            with self.subTest(label):
                proc = subprocess.run(
                    ["cargo", f"+{NIGHTLY}", *args, "--message-format=json"],
                    # cwd matters: cargo finds rust-modules/.cargo/config.toml (the codegen flags)
                    # from the working directory, and a different flag set is a different
                    # fingerprint, i.e. a full rebuild instead of a no-op.
                    cwd=ROOT / "rust-modules",
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
                self.assertEqual(proc.returncode, 0, proc.stderr[-2000:])
                offences = staticlib_artifacts(proc.stdout)
                self.assertEqual(offences, [], f"{label}: {CRATE} still builds a staticlib")


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
