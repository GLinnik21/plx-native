#!/usr/bin/env python3
"""Fail if the app crate rebuilds on a second, identical cargo invocation.

`rust-modules/build.rs` once emitted `cargo:rerun-if-changed=<repo>/RELEASE_LINE` whether or not
that file existed. It does not exist on trunk, and cargo treats a MISSING `rerun-if-changed` path as
stale on every invocation (`CARGO_LOG=cargo::core::compiler::fingerprint=info` prints "stale:
missing .../RELEASE_LINE"). So the build script re-ran every time, its output was re-published, and
the whole ~420k-line app crate recompiled: 30-40 s for a `cargo test --lib --no-run` that changed
nothing, 8 times per `make check`, and again for every lane and every filtered re-run.

Two halves, both reading cargo's own `--message-format=json` records (only lines that parse as JSON
objects with a `reason` are looked at; `cargo test` also prints harness text on stdout):

* `SecondBuildIsFreshTests` builds the real crate twice from `rust-modules/` and demands that the
  second run recompiled nothing of the crate: every `plxnative_modules` (and `plx_platform`, the
  layer crate whose own build script generates the catalog, and `plx_gfx`, whose build script
  compiles the nanosvg object for its own test binary) compiler-artifact is `fresh`, and the
  crate's build script did not write its output again (cargo replays a
  `build-script-executed` record even for a fresh script, so the mtime of the script's output file
  is read, wherever this cargo keeps it).
  It runs `cargo check --lib --tests --features lab-diagnostics` with `CARGO_INCREMENTAL=0`: the
  very invocation `make check`'s lab line has just run, so its first run is a reuse of that unit
  and the whole file costs a few seconds instead of a second cold compile of the crate. (It used
  to run `cargo build --lib`, full codegen, which only avoided a cold compile because
  `test_no_host_staticlib.py` had just built the same unit.) `check` has the same build-script
  fingerprint as `build`, so the always-dirty hazard is identical; it is only cheaper to observe.
  `CARGO_INCREMENTAL` is part of the unit's fingerprint, hence the explicit value: it must match
  the lab line's, whatever the caller exports.

* `MakeAndBareCargoShareAFingerprintTests` builds the real crate the way `make` does (the
  environment `make -s print-cargo-env` reports: every `PLX_*` variable a cargo recipe would see)
  and the way a bare `cargo` does (none), in both orders, and demands the second recompiles
  nothing. Cargo fingerprints an environment variable as an Option, so a variable make exports
  SET-BUT-EMPTY (`PLX_RELEASE=`, `PLX_CHANNEL=`, `PLX_SENTRY_DSN=''`) is a different input from one
  that is unset, and every bare `cargo` (a CI step, rust-analyzer, an agent's `cargo test`) after a
  `make`, and vice versa, recompiled the whole app crate.

* `MarkerStillTriggersTests` keeps the behaviour the always-stale watch was (wrongly) credited with:
  the `RELEASE_LINE` marker appearing, changing and disappearing still re-runs the script, so the
  reported `X.Y.Z-dev` follows the line. It builds a scratch workspace that uses the REAL
  `build.rs` and `src/release_line.rs` against a stub library, in a throwaway repository (the
  appearance is detected through the worktree's `HEAD`, which is what checking out the line moves),
  so the real tree is never touched and nothing app-sized is recompiled. It has no dependencies of
  its own (the catalog and install-identity generators belong to the platform crate's build script
  now) and runs `--offline`.
  It also pins that a missing `.git/logs/HEAD` (reflog off or deleted) does not make the build
  always-dirty, the same hazard through `emit_build_sha`.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
RUST = ROOT / "rust-modules"
CRATE = "plxnative_modules"  # the lib target's name (underscored)
PACKAGE = "plxnative-modules"  # the package's name, as it appears in a package_id
# The layer crate that owns a build script of its own (catalog + install identities): it must not be
# always-dirty either, or every crate above it would rebuild with it.
PLATFORM_CRATE = "plx_platform"
GFX_CRATE = "plx_gfx"
NIGHTLY = os.environ.get("RUST_NIGHTLY", "nightly")


# The invocation `make check`'s lab line runs (Makefile, "cargo check --lib --tests -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx -p plx_net -p plx_ui -p plx_plex -p plx_telemetry -p plx_data -p plx_session -p plx_media -p plx_appkit --features
# lab-diagnostics"); identical arguments and CARGO_INCREMENTAL make the first run here a reuse.
LIB_ARGS = ["check", "--lib", "--tests", "-p", "plxnative-modules", "-p", "plx_base", "-p", "plx_machine", "-p", "plx_platform", "-p", "plx_gfx", "-p", "plx_net", "-p", "plx_ui", "-p", "plx_plex", "-p", "plx_telemetry", "-p", "plx_data", "-p", "plx_session", "-p", "plx_media", "-p", "plx_appkit", "--features", "lab-diagnostics"]


def cargo_env():
    """The caller's environment, but for PATH (which no fingerprint depends on) and
    `CARGO_INCREMENTAL=0`, which the lab line this follows sets and which is part of the unit."""
    env = dict(os.environ)
    env["CARGO_INCREMENTAL"] = "0"
    env["PATH"] = str(Path.home() / ".cargo/bin") + os.pathsep + env.get("PATH", "")
    return env


def json_records(stdout):
    """Every line of `stdout` that is a JSON object carrying a `reason`; harness text is skipped."""
    records = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict) and "reason" in record:
            records.append(record)
    return records


def package_of(record):
    """The package name inside a record's `package_id`, for both the old and the new id spelling."""
    pid = record.get("package_id") or ""
    if "#" in pid:  # `path+file:///…/rust-modules#plxnative-modules@0.7.0` or `…/name#0.7.0`
        head, tail = pid.rsplit("#", 1)
        if "@" in tail:
            return tail.split("@", 1)[0]
        return head.rstrip("/").rsplit("/", 1)[-1]
    return pid.split(" ", 1)[0]


def rebuilt(records):
    """Describe what a build recompiled of the app crate (or of the platform crate, whose build
    script generates the catalog), or [] when it recompiled none of it."""
    return [f"{(rec.get('target') or {}).get('name')} ({'/'.join((rec.get('target') or {}).get('kind') or [])}) was recompiled"
            for rec in records
            if rec.get("reason") == "compiler-artifact"
            and (rec.get("target") or {}).get("name") in (CRATE, PLATFORM_CRATE, GFX_CRATE) and not rec.get("fresh")]


def freeze_stamps(records):
    """Record, on each app-package `build-script-executed` record, when its script last WROTE its
    output, as read NOW.

    cargo replays that record for a fresh script too, so the record alone cannot say whether the
    script ran; its output file (see `script_output_file`) is rewritten exactly when it does.
    Read at once, because the file is the same one the next build will rewrite: a stamp read later
    would show every earlier build the latest mtime and compare equal to itself.
    """
    for rec in records:
        if rec.get("reason") == "build-script-executed" and package_of(rec) == PACKAGE:
            out = script_output_file(rec)
            rec["output_mtime_ns"] = (str(out), out.stat().st_mtime_ns)
    return records


def script_output_file(record):
    """The file cargo wrote the script's stdout to, wherever this cargo keeps it.

    Beside the `out/` directory as `output` in the classic layout (`build/<package>-<hash>/`), as
    `run/stdout` in the newer one (`build/<package>/<hash>/`). Asking the record's own `out_dir`
    rather than globbing a layout keeps this working across cargo versions; finding neither is a
    failure, never an empty stamp that would compare equal to itself.
    """
    base = Path(record["out_dir"]).parent
    for candidate in (base / "output", base / "run" / "stdout"):
        if candidate.exists():
            return candidate
    raise AssertionError(f"no build script output file next to {record['out_dir']}")


def script_stamp(records):
    """`{output file: mtime_ns}` for the app package's build script, as frozen by `freeze_stamps`."""
    return dict(rec["output_mtime_ns"] for rec in records if "output_mtime_ns" in rec)


def saw_crate(records):
    return any(r.get("reason") == "compiler-artifact" and (r.get("target") or {}).get("name") == CRATE
               for r in records)


def run_cargo(args, cwd, env):
    proc = subprocess.run(["cargo", f"+{NIGHTLY}", *args, "--message-format=json"], cwd=cwd,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    return proc, freeze_stamps(json_records(proc.stdout))


class ParserTests(unittest.TestCase):
    def art(self, fresh, name=CRATE):
        return json.dumps({"reason": "compiler-artifact", "fresh": fresh,
                           "target": {"name": name, "kind": ["lib"]}})

    def test_ignores_harness_text_and_broken_lines(self):
        out = "\n".join(["running 0 tests", "{not json", "{}", self.art(True)])
        self.assertEqual(rebuilt(json_records(out)), [])

    def test_stale_crate_artifact_is_an_offence(self):
        self.assertEqual(len(rebuilt(json_records(self.art(False)))), 1)

    def test_other_crates_may_rebuild(self):
        self.assertEqual(rebuilt(json_records(self.art(False, "zstd_sys"))), [])

    def test_script_stamp_only_for_this_package(self):
        with tempfile.TemporaryDirectory() as tmp:
            build = Path(tmp) / "build"
            (build / "out").mkdir(parents=True)
            (build / "output").write_text("cargo:rustc-env=X=1\n")
            ours = json.dumps({"reason": "build-script-executed", "out_dir": str(build / "out"),
                               "package_id": f"path+file:///r/rust-modules#{PACKAGE}@0.7.0"})
            other = json.dumps({"reason": "build-script-executed", "out_dir": str(build / "out"),
                                "package_id": "registry+https://x#serde_json@1.0.0"})
            first = script_stamp(freeze_stamps(json_records(ours)))
            self.assertEqual(list(first), [str(build / "output")])
            self.assertEqual(script_stamp(freeze_stamps(json_records(other))), {})
            # A later rewrite of the file shows up in the NEXT build's stamp, not the earlier one.
            records = freeze_stamps(json_records(ours))
            (build / "output").write_text("cargo:rustc-env=X=2\n")
            self.assertEqual(script_stamp(records), first)
            self.assertNotEqual(script_stamp(freeze_stamps(json_records(ours))), first)

    def test_package_of_old_spelling(self):
        rec = {"package_id": f"{PACKAGE} 0.7.0 (path+file:///r/rust-modules)"}
        self.assertEqual(package_of(rec), PACKAGE)


class SecondBuildIsFreshTests(unittest.TestCase):
    def test_second_identical_build_recompiles_nothing(self):
        # cwd matters: cargo finds rust-modules/.cargo/config.toml (the codegen flags) from the
        # working directory, and a different flag set is a different fingerprint.
        env = cargo_env()
        first, first_records = run_cargo(LIB_ARGS, RUST, env)
        self.assertEqual(first.returncode, 0, first.stderr[-2000:])
        second, records = run_cargo(LIB_ARGS, RUST, env)
        self.assertEqual(second.returncode, 0, second.stderr[-2000:])
        self.assertTrue(saw_crate(records), "no compiler-artifact record for the app crate at all")
        self.assertTrue(script_stamp(records), "no build-script-executed record for the app crate")
        why = ("an identical second build must be fresh — a build script that emits "
               "rerun-if-changed for a path that does not exist is stale every time")
        self.assertEqual(rebuilt(records), [], why)
        self.assertEqual(script_stamp(records), script_stamp(first_records),
                         "the build script ran again: " + why)


def hermetic_env():
    """`cargo_env()` without any `PLX_*`: the baseline a bare `cargo` in a clean shell sees."""
    return {k: v for k, v in cargo_env().items() if not k.startswith("PLX_")}


def make_cargo_env():
    """The `PLX_*` environment `make` hands a cargo recipe, as `{name: value}`.

    Asked of the Makefile itself (`print-cargo-env`: its exported variables plus the telemetry words
    the recipes put in front of `cargo`) rather than restated here, so this follows the recipes.
    Run from a `PLX_*`-free environment, and with the telemetry file pointed at nothing, so what
    comes back is the Makefile's own construction and not this machine's credentials.
    """
    proc = subprocess.run(["make", "-s", "print-cargo-env", "TELEMETRY_JSON=/dev/null"], cwd=ROOT,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                          env=hermetic_env())
    if proc.returncode != 0:
        raise AssertionError("make -s print-cargo-env failed: " + proc.stderr[-2000:])
    return dict(line.split("=", 1) for line in proc.stdout.splitlines() if "=" in line)


class MakeAndBareCargoShareAFingerprintTests(unittest.TestCase):
    def test_make_exports_no_plx_variable_set_but_empty(self):
        empty = sorted(k for k, v in make_cargo_env().items() if v == "")
        self.assertEqual(empty, [], "make hands cargo these variables set-but-EMPTY, which cargo "
                         "fingerprints differently from unset; export them only when non-empty")

    def test_make_then_bare_cargo_recompiles_nothing(self):
        make_env = {**hermetic_env(), **make_cargo_env()}
        bare_env = hermetic_env()
        first, _ = run_cargo(LIB_ARGS, RUST, bare_env)
        self.assertEqual(first.returncode, 0, first.stderr[-2000:])
        for label, env, prev_env in (("make", make_env, bare_env), ("bare cargo", bare_env, make_env)):
            before, _ = run_cargo(LIB_ARGS, RUST, prev_env)
            self.assertEqual(before.returncode, 0, before.stderr[-2000:])
            after, records = run_cargo(LIB_ARGS, RUST, env)
            self.assertEqual(after.returncode, 0, after.stderr[-2000:])
            self.assertTrue(saw_crate(records), "no compiler-artifact record for the app crate")
            self.assertEqual(rebuilt(records), [],
                             f"{label} recompiled the crate after the other kind of invocation: "
                             "make and bare cargo must present the same environment fingerprint")


STUB_MANIFEST = """\
[package]
name = "plxnative-modules"
version = "0.7.0"
edition = "2021"
build = "build.rs"

[lib]
path = "src/lib.rs"
"""


def vcs(cwd, *args):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                    "-c", "commit.gpgsign=false", *args], cwd=cwd, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


class MarkerStillTriggersTests(unittest.TestCase):
    """The RELEASE_LINE marker appearing, changing and disappearing must still re-run build.rs."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="plx-release-line-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        repo = self.tmp / "repo"
        crate = repo / "rust-modules"
        (crate / "src").mkdir(parents=True)
        # The REAL script and the sources it includes, byte for byte.
        shutil.copy(RUST / "build.rs", crate / "build.rs")
        shutil.copy(RUST / "src/release_line.rs", crate / "src/release_line.rs")
        # `build.rs` includes the shared host link configuration by `#[path]`.
        (crate / "build_support").mkdir()
        shutil.copy(RUST / "build_support/host_link.rs", crate / "build_support/host_link.rs")
        (crate / "src/lib.rs").write_text("// stub library for the build-script freshness test\n")
        (crate / "Cargo.toml").write_text(STUB_MANIFEST)
        shutil.copy(RUST / "Cargo.lock", crate / "Cargo.lock")
        # Inputs the script reads but this test never edits.
        for name in ("src", "vendor"):
            (repo / name).symlink_to(ROOT / name)
        self.repo, self.crate = repo, crate
        vcs(repo, "init", "-q", "-b", "trunk")
        (repo / ".gitignore").write_text("target/\n")
        vcs(repo, "add", ".gitignore", "rust-modules")
        vcs(repo, "commit", "-q", "-m", "trunk")
        # A throwaway target directory of its own: no multi-gigabyte incremental cache for it.
        # Hermetic against the caller: `PLX_*` (PLX_RELEASE, PLX_CHANNEL, ...) would change the
        # version the script publishes, `GIT_*` would redirect the repository it asks.
        self.env = {k: v for k, v in cargo_env().items() if not k.startswith(("PLX_", "GIT_"))}
        self.env.update(CARGO_TARGET_DIR=str(self.tmp / "target"), CARGO_INCREMENTAL="0")
        self.last = []

    def build(self):
        proc, records = run_cargo(["build", "--lib", "--offline"], self.crate, self.env)
        self.assertEqual(proc.returncode, 0, proc.stderr[-2000:])
        self.last = records
        return records

    def version(self):
        """The PLX_VERSION the script published on the latest build.

        Found through the `out_dir` of cargo's own `build-script-executed` record rather than by
        globbing `target/debug/build/<package>-*/`: newer cargo lays the build directory out as
        `build/<package>/<hash>/`, and a glob over one layout finds nothing under the other.
        """
        for output in script_stamp(self.last):
            for line in Path(output).read_text().splitlines():
                if line.startswith(("cargo:rustc-env=PLX_VERSION=", "cargo::rustc-env=PLX_VERSION=")):
                    return line.split("=", 2)[2]
        self.fail("no PLX_VERSION in the build script output of the latest build")

    def assertFresh(self, before, after, why):
        self.assertTrue(saw_crate(after), why)
        self.assertEqual(rebuilt(after), [], why)
        self.assertEqual(script_stamp(after), script_stamp(before), why)

    def assertRebuilt(self, before, after, why):
        self.assertNotEqual(script_stamp(after), script_stamp(before), why)
        self.assertNotEqual(rebuilt(after), [], why)

    def test_marker_transitions_rerun_the_script(self):
        prev = self.build()
        self.assertEqual(self.version(), "0.8.0-dev")
        cur = self.build()
        self.assertFresh(prev, cur, "trunk (no marker): an identical second build is fresh")

        # absent -> present, the way a maintenance line arrives: a commit that adds the marker,
        # checked out from trunk.
        vcs(self.repo, "checkout", "-q", "-b", "release/v0.7")
        (self.repo / "RELEASE_LINE").write_text("0.7\n")
        vcs(self.repo, "add", "RELEASE_LINE")
        vcs(self.repo, "commit", "-q", "-m", "line")
        vcs(self.repo, "checkout", "-q", "trunk")
        prev = self.build()
        self.assertEqual(self.version(), "0.8.0-dev")
        vcs(self.repo, "checkout", "-q", "release/v0.7")
        cur = self.build()
        self.assertRebuilt(prev, cur, "checking out a line that carries the marker must rebuild")
        self.assertEqual(self.version(), "0.7.1-dev")
        prev, cur = cur, self.build()
        self.assertFresh(prev, cur, "on the line, an identical second build is fresh")

        # present -> changed in place
        (self.repo / "RELEASE_LINE").write_text("0.9\n")
        prev, cur = cur, self.build()
        self.assertRebuilt(prev, cur, "editing the marker must rebuild")
        self.assertEqual(self.version(), "0.9.1-dev")
        prev, cur = cur, self.build()
        self.assertFresh(prev, cur, "after the edit, an identical second build is fresh")

        # present -> absent, back to trunk
        vcs(self.repo, "checkout", "-q", "-f", "trunk")
        prev, cur = cur, self.build()
        self.assertRebuilt(prev, cur, "leaving the line (marker removed) must rebuild")
        self.assertEqual(self.version(), "0.8.0-dev")
        prev, cur = cur, self.build()
        self.assertFresh(prev, cur, "back on trunk, an identical second build is fresh")

    def test_missing_reflog_does_not_make_the_build_dirty(self):
        self.build()
        (self.repo / ".git/logs/HEAD").unlink()
        self.build()  # the watched file vanished: one legitimate rerun
        prev, cur = self.build(), self.build()
        self.assertFresh(prev, cur, "no .git/logs/HEAD: an identical second build is fresh")


if __name__ == "__main__":
    sys.exit(unittest.main(verbosity=1))
