#!/usr/bin/env python3
"""`make build-bench`: a repeatable local build benchmark, so a build-affecting change can paste a
before/after table instead of an ad-hoc scratch-script number.

Scenarios (each a named row; `--only ID,ID` selects, `--quick` is `noop,leaf,sizes` at one run):

  noop      host test build with nothing changed (`cargo test --lib --no-run`); reports whether the
            app crate was rebuilt, read from cargo's own `--message-format=json` `fresh` flag.
  leaf      the same build after appending a comment to a leaf file of the BASE layer crate
            (rust-modules/base/src/cbuf.rs, in `plx_base`): the crate every other layer depends on,
            so the app crate rebuilds behind it. This was the one-crate leaf edit before the split.
  machine   ...after appending one to a leaf file of the MACHINE layer crate
            (rust-modules/machine/src/landgate.rs, in `plx_machine`): `plx_base` stays fresh, the
            machine crate and the app crate behind it recompile.
  platform  ...after appending one to a leaf file of the PLATFORM layer crate
            (rust-modules/platform/src/devcaps.rs, in `plx_platform`): `plx_base` and `plx_machine`
            stay fresh, the platform crate and the app crate behind it recompile.
  gfx       ...after appending one to a leaf file of the GFX layer crate
            (rust-modules/gfx/src/overdraw.rs, in `plx_gfx`): `plx_base` and `plx_machine` stay
            fresh, the gfx crate and the app crate behind it recompile.
  net       ...after appending one to a leaf file of the NET layer crate
            (rust-modules/net/src/stream_redirect.rs, in `plx_net`): `plx_base` stays fresh, the net
            crate and the app crate behind it recompile.
  ui        ...after appending one to a leaf file of the UI layer crate
            (rust-modules/ui/src/dwell.rs, in `plx_ui`): `plx_base`, `plx_machine`, `plx_platform`
            and `plx_gfx` stay fresh, the ui crate and the app crate behind it recompile.
  plex      ...after appending one to a leaf file of the PLEX layer crate
            (rust-modules/plex/src/plex/retry.rs, in `plx_plex`): `plx_base`, `plx_machine`,
            `plx_platform` and `plx_net` stay fresh, the plex crate and the app crate behind it
            recompile.
  telemetry ...after appending one to a leaf file of the TELEMETRY layer crate
            (rust-modules/telemetry/src/telemetry/window.rs, in `plx_telemetry`): `plx_base`,
            `plx_platform`, `plx_net` and `plx_plex` stay fresh, the telemetry crate and the app
            crate behind it recompile.
  app       ...after appending one to a leaf file of the app crate (rust-modules/src/coldstart.rs):
            `plx_base` stays fresh and only the app crate recompiles.
  hub       ...after appending one to a hub file (rust-modules/ui/src/lib.rs, the root of `plx_ui`,
            which every widget and screen in the app crate names).
  leaf-inc  `leaf` with CARGO_INCREMENTAL=1 in rust-modules/target-fast (the `make test-fast` tree).
  hub-inc   `hub`, incremental. The two -inc rows are skipped with a note when target-fast is
            absent, unless `--cold` (which builds it, untimed, first).
  tests     the default-feature unit suite (`cargo test --lib -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx -p plx_net -p plx_ui -p plx_plex -p plx_telemetry`) on
            a warm tree; the `test result:` counts of every crate are summed.
  arm       the ARM staticlib line (`cargo rustc ... --crate-type staticlib`) after touching lib.rs,
            then the archive's size and sha256. Skipped when the ARM archive was never built here;
            the FFmpeg build is never triggered.
  sizes     `du` of rust-modules/target*, and how many target dirs there are. Not timed.

Every timed scenario runs `--runs` times (default 3), INTERLEAVED (one round runs each scenario in
turn), and is reported as median / min / max. The host's load average and swap are sampled before
every run and a WARNING is printed when load exceeds the core count or swap is over 90% full: on a
shared Mac that makes every number noisy. (Load right after a build includes that build, so a
warning from the second round on is a hint, not a verdict.)

The cargo invocations are NOT restated here. `make -s print-bench-config` (Makefile) hands over the
toolchain, target dir, feature flags and RUSTFLAGS the real recipes use; only the cargo subcommand
skeleton below is mirrored, and ci/test_build_bench.py fails when it drifts from the Makefile's
recipes. Run it through `make build-bench`, which also exports the same environment `make
test-fast` gives cargo (a bare cargo and a make-driven one fingerprint differently) and queues on
the machine-wide `make check` lock (tools/check-lock.py) so two builds never skew each other.

Safety: refuses under RELEASE=1; never touches the TV; never cleans a target dir; edits only
EDIT-TARGET files, refuses to start if one has uncommitted changes, restores the original bytes in
a `finally` (SIGTERM/SIGHUP included) and verifies `git diff --quiet` on them at the end.

Exit status: 0 ok; 1 a scenario failed (the table of what ran is still printed); 2 refused to start;
3 an edit target did not verify clean at the end.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

LEAF_FILE = "rust-modules/base/src/cbuf.rs"
MACHINE_LEAF_FILE = "rust-modules/machine/src/landgate.rs"
PLATFORM_LEAF_FILE = "rust-modules/platform/src/devcaps.rs"
GFX_LEAF_FILE = "rust-modules/gfx/src/overdraw.rs"
NET_LEAF_FILE = "rust-modules/net/src/stream_redirect.rs"
UI_LEAF_FILE = "rust-modules/ui/src/dwell.rs"
PLEX_LEAF_FILE = "rust-modules/plex/src/plex/retry.rs"
TELEMETRY_LEAF_FILE = "rust-modules/telemetry/src/telemetry/window.rs"
APP_LEAF_FILE = "rust-modules/src/coldstart.rs"
HUB_FILE = "rust-modules/ui/src/lib.rs"
TOUCH_FILE = "rust-modules/src/lib.rs"
APP_PACKAGE = "plxnative-modules"
BASE_PACKAGE = "plx_base"
MACHINE_PACKAGE = "plx_machine"
PLATFORM_PACKAGE = "plx_platform"
GFX_PACKAGE = "plx_gfx"
NET_PACKAGE = "plx_net"
UI_PACKAGE = "plx_ui"
PLEX_PACKAGE = "plx_plex"
TELEMETRY_PACKAGE = "plx_telemetry"

# The cargo subcommand skeletons the Makefile's recipes use. Everything else (toolchain, dirs,
# feature flags, RUSTFLAGS) comes from `make -s print-bench-config`. ci/test_build_bench.py compares
# these to the recipes in the Makefile.
HOST_TEST_ARGS = ("test", "--lib", "-p", "plxnative-modules", "-p", "plx_base", "-p", "plx_machine", "-p", "plx_platform", "-p", "plx_gfx", "-p", "plx_net", "-p", "plx_ui", "-p", "plx_plex", "-p", "plx_telemetry")
HOST_TEST_BUILD_ARGS = HOST_TEST_ARGS + ("--no-run", "--message-format=json")
ARM_ARGS_HEAD = ("rustc", "--release", "--target")  # then the target triple
ARM_ARGS_LIB = ("--lib", "--crate-type", "staticlib", "--target-dir")  # then the target dir
ARM_ARGS_TAIL = ("--message-format=json-render-diagnostics",)  # after the feature flags

ALL_SCENARIOS = ("noop", "leaf", "machine", "platform", "gfx", "net", "ui", "plex", "telemetry", "app", "hub", "leaf-inc", "hub-inc", "tests", "arm", "sizes")
QUICK_SCENARIOS = ("noop", "leaf", "sizes")
TITLES = {
    "noop": "No-op host test build",
    "leaf": "Edit leaf (plx_base cbuf.rs), non-incremental",
    "machine": "Edit leaf (plx_machine landgate.rs), non-incremental",
    "platform": "Edit leaf (plx_platform devcaps.rs), non-incremental",
    "gfx": "Edit leaf (plx_gfx overdraw.rs), non-incremental",
    "net": "Edit leaf (plx_net stream_redirect.rs), non-incremental",
    "ui": "Edit leaf (plx_ui dwell.rs), non-incremental",
    "plex": "Edit leaf (plx_plex retry.rs), non-incremental",
    "telemetry": "Edit leaf (plx_telemetry window.rs), non-incremental",
    "app": "Edit leaf (app coldstart.rs), non-incremental",
    "hub": "Edit hub (plx_ui lib.rs), non-incremental",
    "leaf-inc": "Edit leaf (plx_base cbuf.rs), incremental",
    "hub-inc": "Edit hub (plx_ui lib.rs), incremental",
    "tests": "Unit suite run (default features)",
    "arm": "ARM staticlib rebuild (touch lib.rs)",
    "sizes": "Target dir sizes",
}
SWAP_WARN_PCT = 90.0
EDIT_FILES = {"leaf": LEAF_FILE, "leaf-inc": LEAF_FILE, "machine": MACHINE_LEAF_FILE, "platform": PLATFORM_LEAF_FILE, "gfx": GFX_LEAF_FILE, "net": NET_LEAF_FILE, "ui": UI_LEAF_FILE, "plex": PLEX_LEAF_FILE, "telemetry": TELEMETRY_LEAF_FILE, "app": APP_LEAF_FILE, "hub": HUB_FILE, "hub-inc": HUB_FILE}


class BenchError(Exception):
    """A scenario could not produce a number; the run stops, prints what it has, exits 1."""


class Refused(Exception):
    """The run must not start (exit 2)."""


def log(msg: str) -> None:
    print(f"build-bench: {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------- configuration from the Makefile

def read_make_config(repo: Path) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k not in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL")}
    proc = subprocess.run(["make", "-s", "print-bench-config"], cwd=repo, env=env,
                          capture_output=True, text=True)
    if proc.returncode != 0:
        raise Refused("`make -s print-bench-config` failed: " + proc.stderr.strip()[-400:])
    cfg = dict(line.split("=", 1) for line in proc.stdout.splitlines() if "=" in line)
    missing = [k for k in ("RUST_NIGHTLY", "RUST_TDIR", "RUST_TARGET", "RUST_LIB", "RUST_ENV",
                           "TEST_FAST_TDIR") if k not in cfg]
    if missing:
        raise Refused("print-bench-config did not report: " + ", ".join(missing))
    cfg.setdefault("RUST_FEATFLAGS", "")
    cfg.setdefault("RELEASE", "")
    return cfg


def arm_argv(cfg: dict[str, str]) -> list[str]:
    """The ARM staticlib cargo argv, in the Makefile's order, after `cargo +<nightly>`."""
    return [*ARM_ARGS_HEAD, cfg["RUST_TARGET"], *ARM_ARGS_LIB, cfg["RUST_TDIR"],
            *shlex.split(cfg.get("RUST_FEATFLAGS", "")), *ARM_ARGS_TAIL]


# ---------------------------------------------------------------- machine state

def parse_load(uptime_out: str) -> float | None:
    m = re.search(r"load averages?:\s*([\d.]+)", uptime_out)
    return float(m.group(1)) if m else None


def _mb(value: str, unit: str) -> float:
    return float(value) * {"K": 1 / 1024, "M": 1.0, "G": 1024.0, "T": 1024.0 * 1024}[unit.upper()]


def parse_swap_pct(swapusage: str) -> float | None:
    """`total = 4096.00M  used = 3048.88M  free = 1047.12M` -> used / total in percent."""
    m = re.search(r"total = ([\d.]+)([KMGT]).*?used = ([\d.]+)([KMGT])", swapusage)
    if not m:
        return None
    total, used = _mb(m.group(1), m.group(2)), _mb(m.group(3), m.group(4))
    return round(100.0 * used / total, 1) if total > 0 else 0.0


def sample_machine(env: dict[str, str]) -> dict:
    def out(*cmd: str) -> str:
        try:
            return subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=20).stdout
        except (OSError, subprocess.SubprocessError):
            return ""
    return {"load1": parse_load(out("uptime")),
            "swap_used_pct": parse_swap_pct(out("sysctl", "-n", "vm.swapusage"))}


def noise_warnings(state: dict, cores: int) -> list[str]:
    ws = []
    if state.get("load1") is not None and state["load1"] > cores:
        ws.append(f"load {state['load1']:.2f} > {cores} cores")
    if state.get("swap_used_pct") is not None and state["swap_used_pct"] > SWAP_WARN_PCT:
        ws.append(f"swap {state['swap_used_pct']:.0f}% used")
    return ws


# ---------------------------------------------------------------- cargo plumbing

class Cargo:
    def __init__(self, repo: Path, cfg: dict[str, str]):
        self.cfg = cfg
        self.cwd = repo / "rust-modules"
        home = os.environ.get("HOME", "")
        self.base_env = dict(os.environ)
        self.base_env["PATH"] = f"{home}/.cargo/bin:{os.environ.get('PATH', '')}"
        self.base_env.pop("CARGO_TARGET_DIR", None)
        self.runtime_dir = tempfile.mkdtemp(prefix="plxnative-bench.")
        self.base_env["PLXNATIVE_RUNTIME_DIR"] = self.runtime_dir
        self.toolchain = f"+{cfg['RUST_NIGHTLY']}"

    def env(self, incremental: bool) -> dict[str, str]:
        e = dict(self.base_env)
        e["CARGO_INCREMENTAL"] = "1" if incremental else "0"
        if incremental:
            e["CARGO_TARGET_DIR"] = self.cfg["TEST_FAST_TDIR"]
        return e

    def run(self, args, env, what: str, check: bool = True):
        t0 = time.monotonic()
        proc = subprocess.run(["cargo", self.toolchain, *args], cwd=self.cwd, env=env,
                              capture_output=True, text=True)
        secs = time.monotonic() - t0
        if check and proc.returncode != 0:
            raise BenchError(f"{what}: cargo exited {proc.returncode}: {proc.stderr.strip()[-600:]}")
        return secs, proc

    def build(self, incremental: bool, what: str):
        """`cargo test --lib --no-run` with JSON on stdout -> (seconds, build summary)."""
        secs, proc = self.run(HOST_TEST_BUILD_ARGS, self.env(incremental), what)
        return secs, summarize_artifacts(proc.stdout)

    def toolchain_version(self) -> str:
        try:
            p = subprocess.run(["rustc", self.toolchain, "-V"], cwd=self.cwd, env=self.base_env,
                               capture_output=True, text=True, timeout=60)
            return p.stdout.strip() or "unknown"
        except (OSError, subprocess.SubprocessError):
            return "unknown"


def summarize_artifacts(stdout: str) -> dict:
    """Read cargo's JSON records: how many units, how many were rebuilt, was the app crate."""
    total = rebuilt = 0
    app_rebuilt = base_rebuilt = machine_rebuilt = platform_rebuilt = gfx_rebuilt = net_rebuilt = ui_rebuilt = plex_rebuilt = telemetry_rebuilt = False
    for line in stdout.splitlines():
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if not isinstance(rec, dict) or rec.get("reason") != "compiler-artifact":
            continue
        total += 1
        if not rec.get("fresh", False):
            rebuilt += 1
            app_rebuilt = app_rebuilt or APP_PACKAGE in str(rec.get("package_id", ""))
            base_rebuilt = base_rebuilt or BASE_PACKAGE in str(rec.get("package_id", ""))
            machine_rebuilt = machine_rebuilt or MACHINE_PACKAGE in str(rec.get("package_id", ""))
            platform_rebuilt = platform_rebuilt or PLATFORM_PACKAGE in str(rec.get("package_id", ""))
            gfx_rebuilt = gfx_rebuilt or GFX_PACKAGE in str(rec.get("package_id", ""))
            net_rebuilt = net_rebuilt or NET_PACKAGE in str(rec.get("package_id", ""))
            ui_rebuilt = ui_rebuilt or UI_PACKAGE in str(rec.get("package_id", ""))
            plex_rebuilt = plex_rebuilt or PLEX_PACKAGE in str(rec.get("package_id", ""))
            telemetry_rebuilt = telemetry_rebuilt or TELEMETRY_PACKAGE in str(rec.get("package_id", ""))
    return {"units": total, "rebuilt": rebuilt, "app_rebuilt": app_rebuilt, "base_rebuilt": base_rebuilt,
            "machine_rebuilt": machine_rebuilt, "platform_rebuilt": platform_rebuilt,
            "gfx_rebuilt": gfx_rebuilt, "net_rebuilt": net_rebuilt, "ui_rebuilt": ui_rebuilt,
            "plex_rebuilt": plex_rebuilt, "telemetry_rebuilt": telemetry_rebuilt}


def parse_test_result(text: str) -> dict | None:
    m = re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored", text)
    if not m:
        return None
    # One `test result:` line per test binary (the app crate, then each layer crate): the suite is their sum.
    status = "ok" if all(s == "ok" for s, *_ in m) else "FAILED"
    return {"status": status, "passed": sum(int(r[1]) for r in m), "failed": sum(int(r[2]) for r in m),
            "ignored": sum(int(r[3]) for r in m)}


# ---------------------------------------------------------------- edit guard

class EditGuard:
    """Appends a comment to a tracked file and restores the original bytes no matter what."""

    def __init__(self, repo: Path):
        self.repo = repo
        self.counter = 0

    def check_clean(self, rels) -> None:
        for rel in rels:
            if not (self.repo / rel).is_file():
                raise Refused(f"edit target {rel} does not exist")
        if not rels:
            return
        proc = subprocess.run(["git", "status", "--porcelain", "--", *rels], cwd=self.repo,
                              capture_output=True, text=True)
        if proc.returncode != 0:
            raise Refused("git status failed: " + proc.stderr.strip()[-300:])
        if proc.stdout.strip():
            raise Refused("refusing to start: the benchmark edits these files and restores them, "
                          "so they must be clean first:\n" + proc.stdout.rstrip())

    @contextlib.contextmanager
    def edited(self, rel: str):
        path = self.repo / rel
        original = path.read_bytes()
        self.counter += 1
        try:
            suffix = b"" if original.endswith(b"\n") else b"\n"
            path.write_bytes(original + suffix + f"// build-bench edit {self.counter}\n".encode())
            yield
        finally:
            path.write_bytes(original)

    def verify_clean(self, rels) -> list[str]:
        return [rel for rel in rels
                if subprocess.run(["git", "diff", "--quiet", "--", rel], cwd=self.repo).returncode != 0]


# ---------------------------------------------------------------- the benchmark

class Bench:
    def __init__(self, repo: Path, cfg: dict[str, str], scenarios: list[str], runs: int,
                 cold: bool, cores: int):
        self.repo, self.cfg, self.runs, self.cold, self.cores = repo, cfg, runs, cold, cores
        self.cargo = Cargo(repo, cfg)
        self.guard = EditGuard(repo)
        self.requested = scenarios
        self.warm = {False: False, True: False}  # incremental? -> tree built since its last change
        self.results: dict[str, dict] = {}
        self.sizes: dict | None = None
        self.warnings: list[str] = []
        self.failure: str | None = None

    def skip_reason(self, sid: str) -> str | None:
        if sid.endswith("-inc") and not self.cold \
                and not (self.cargo.cwd / self.cfg["TEST_FAST_TDIR"]).is_dir():
            return (f"rust-modules/{self.cfg['TEST_FAST_TDIR']} is absent (run `make test-fast` "
                    "once or pass --cold to build it first)")
        if sid == "arm" and not (self.repo / self.cfg["RUST_LIB"]).is_file():
            return (f"no ARM staticlib at {self.cfg['RUST_LIB']}; build it once with `make` "
                    "(the benchmark never triggers the FFmpeg build)")
        return None

    def plan(self) -> list[str]:
        active = []
        for sid in self.requested:
            note = self.skip_reason(sid)
            self.results[sid] = {"id": sid, "name": TITLES[sid], "status": "skipped" if note else "ok",
                                 "note": note or "", "samples": []}
            if not note and sid != "sizes":
                active.append(sid)
        return active

    def ensure_warm(self, incremental: bool) -> None:
        if not self.warm[incremental]:
            log(f"warming the {'incremental' if incremental else 'non-incremental'} test build (untimed)")
            self.cargo.build(incremental, "warm-up build")
            self.warm[incremental] = True

    def run_once(self, sid: str) -> dict:
        state = sample_machine(self.cargo.base_env)
        sample = dict(state, warnings=noise_warnings(state, self.cores))
        if sid == "noop":
            self.ensure_warm(False)
            secs, b = self.cargo.build(False, "no-op build")
            sample.update(seconds=secs, **b)
        elif sid in EDIT_FILES:
            inc = sid.endswith("-inc")
            self.ensure_warm(inc)
            with self.guard.edited(EDIT_FILES[sid]):
                self.warm = dict.fromkeys(self.warm, False)  # the restored file's new mtime stales BOTH trees
                secs, b = self.cargo.build(inc, f"{sid} edit-rebuild")
            sample.update(seconds=secs, **b)
        elif sid == "tests":
            self.ensure_warm(False)
            secs, proc = self.cargo.run(HOST_TEST_ARGS, self.cargo.env(False), "unit suite", check=False)
            result = parse_test_result(proc.stdout + proc.stderr)
            if result is None:
                raise BenchError(f"unit suite: no `test result:` line (cargo exited "
                                 f"{proc.returncode}): {proc.stderr.strip()[-400:]}")
            sample.update(seconds=secs, **result)
        elif sid == "arm":
            sample.update(self.run_arm())
        else:
            raise BenchError(f"unknown scenario {sid}")
        return sample

    def run_arm(self) -> dict:
        env = dict(self.cargo.env(False))
        for word in shlex.split(self.cfg["RUST_ENV"]):
            if "=" in word:
                k, v = word.split("=", 1)
                env[k] = v
        os.utime(self.repo / TOUCH_FILE, None)
        secs, _ = self.cargo.run(arm_argv(self.cfg), env, "ARM staticlib rebuild")
        lib = self.repo / self.cfg["RUST_LIB"]
        if not lib.is_file():
            raise BenchError(f"ARM staticlib rebuild: {self.cfg['RUST_LIB']} missing after cargo succeeded")
        return {"seconds": secs, "archive_bytes": lib.stat().st_size,
                "archive_sha256": hashlib.sha256(lib.read_bytes()).hexdigest()}

    def measure_sizes(self) -> dict:
        dirs = sorted(p for p in (self.repo / "rust-modules").glob("target*") if p.is_dir())
        entries = []
        for d in dirs:
            proc = subprocess.run(["du", "-sk", str(d)], capture_output=True, text=True)
            try:
                kb = int(proc.stdout.split()[0])
            except (IndexError, ValueError):
                kb = 0
            entries.append({"dir": d.name, "mb": round(kb / 1024.0, 1)})
        return {"dirs": entries, "count": len(entries), "total_mb": round(sum(e["mb"] for e in entries), 1)}

    def execute(self) -> None:
        active = self.plan()
        try:
            for rnd in range(self.runs):
                for sid in active:
                    log(f"round {rnd + 1}/{self.runs}: {sid}")
                    sample = self.run_once(sid)
                    self.results[sid]["samples"].append(sample)
                    self.warnings += [f"{sid} run {rnd + 1}: {w}" for w in sample["warnings"]]
        except BenchError as e:
            self.failure = str(e)
            log(f"FAILED: {e}")
            for sid in active:
                if len(self.results[sid]["samples"]) < self.runs:
                    self.results[sid]["status"] = "incomplete" if self.results[sid]["samples"] else "failed"
        if "sizes" in self.requested:
            self.sizes = self.measure_sizes()


# ---------------------------------------------------------------- reporting

def stats(samples: list[dict]) -> dict:
    secs = [s["seconds"] for s in samples if "seconds" in s]
    if not secs:
        return {}
    return {"median": round(statistics.median(secs), 2), "min": round(min(secs), 2),
            "max": round(max(secs), 2)}


def row_info(sid: str, res: dict) -> str:
    samples = res["samples"]
    if res["status"] == "skipped":
        return res["note"]
    if not samples:
        return res["status"]
    last = samples[-1]
    if sid in EDIT_FILES or sid == "noop":
        flags = {s["app_rebuilt"] for s in samples}
        which = "yes" if flags == {True} else "no" if flags == {False} else "mixed"
        bflags = {s.get("base_rebuilt", False) for s in samples}
        bwhich = "yes" if bflags == {True} else "no" if bflags == {False} else "mixed"
        mflags = {s.get("machine_rebuilt", False) for s in samples}
        mwhich = "yes" if mflags == {True} else "no" if mflags == {False} else "mixed"
        pflags = {s.get("platform_rebuilt", False) for s in samples}
        pwhich = "yes" if pflags == {True} else "no" if pflags == {False} else "mixed"
        gflags = {s.get("gfx_rebuilt", False) for s in samples}
        gwhich = "yes" if gflags == {True} else "no" if gflags == {False} else "mixed"
        nflags = {s.get("net_rebuilt", False) for s in samples}
        nwhich = "yes" if nflags == {True} else "no" if nflags == {False} else "mixed"
        uflags = {s.get("ui_rebuilt", False) for s in samples}
        uwhich = "yes" if uflags == {True} else "no" if uflags == {False} else "mixed"
        xflags = {s.get("plex_rebuilt", False) for s in samples}
        xwhich = "yes" if xflags == {True} else "no" if xflags == {False} else "mixed"
        tflags = {s.get("telemetry_rebuilt", False) for s in samples}
        twhich = "yes" if tflags == {True} else "no" if tflags == {False} else "mixed"
        text = (f"app crate rebuilt: {which}, plx_base rebuilt: {bwhich}, plx_machine rebuilt: {mwhich}, "
                f"plx_platform rebuilt: {pwhich}, plx_gfx rebuilt: {gwhich}, plx_net rebuilt: {nwhich}, plx_ui rebuilt: {uwhich}, plx_plex rebuilt: {xwhich}, plx_telemetry rebuilt: {twhich} "
                f"({last['rebuilt']}/{last['units']} units)")
        if sid == "noop" and True in flags:
            text += " **UNEXPECTED: a no-op build recompiled the app crate**"
        return text
    if sid == "tests":
        text = f"{last['passed']} passed / {last['failed']} failed / {last['ignored']} ignored"
        return text + (" **FAILURES**" if last["failed"] else "")
    if sid == "arm":
        return f"archive {last['archive_bytes'] / 1048576:.1f} MiB, sha256 {last['archive_sha256'][:16]}"
    return ""


def scenario_docs(bench: Bench) -> list[dict]:
    docs = []
    for sid in ALL_SCENARIOS:
        res = bench.results.get(sid)
        if res is not None:
            docs.append({**res, **stats(res["samples"]), "info": row_info(sid, res)})
    return docs


def render_markdown(doc: dict) -> str:
    h = doc["host"]
    lines = [
        f"**build-bench** `{doc['git_sha']}`{' (dirty tree)' if doc['tree_dirty'] else ''} · "
        f"{doc['toolchain']} · {h['machine']}, {h['cores']} cores, {h['os']} · "
        f"{doc['runs']} run(s) per scenario, interleaved",
        "",
        "| Scenario | Runs | Median | Min | Max | Notes |",
        "|---|---|---|---|---|---|",
    ]
    for sc in doc["scenarios"]:
        if sc["id"] == "sizes":
            continue
        if sc["status"] == "skipped":
            lines.append(f"| {sc['name']} | - | skipped | | | {sc['note']} |")
        elif "median" not in sc:
            lines.append(f"| {sc['name']} | 0 | {sc['status']} | | | {sc['info']} |")
        else:
            lines.append(f"| {sc['name']} | {len(sc['samples'])} | {sc['median']:.1f} s | "
                         f"{sc['min']:.1f} s | {sc['max']:.1f} s | {sc['info']} |")
    if doc.get("sizes"):
        s = doc["sizes"]
        lines += ["", f"| Target dirs ({s['count']}) | Size |", "|---|---|"]
        lines += [f"| rust-modules/{e['dir']} | {e['mb']:.0f} MB |" for e in s["dirs"]]
        lines.append(f"| total | {s['total_mb']:.0f} MB |")
    if doc["warnings"]:
        lines.append("")
        lines += [f"WARNING: {w}" for w in doc["warnings"]]
        lines += ["", "WARNING: the host was loaded (load above core count or swap over 90% full): "
                  "these numbers are noisy; compare medians of repeated runs only."]
    if doc.get("failure"):
        lines += ["", f"FAILED: {doc['failure']}"]
    return "\n".join(lines)


def git_out(repo: Path, *args: str) -> str:
    try:
        return subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True).stdout.strip()
    except OSError:
        return ""


def host_info(cores: int) -> dict:
    cpu = ""
    if platform.system() == "Darwin":
        with contextlib.suppress(OSError, subprocess.SubprocessError):
            cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True,
                                 text=True, timeout=10).stdout.strip()
    return {"os": f"{platform.system()} {platform.release()}", "machine": cpu or platform.machine(),
            "cores": cores}


# ---------------------------------------------------------------- main

def parse_args(argv):
    p = argparse.ArgumentParser(prog="build-bench", description="Repeatable local build benchmark.")
    p.add_argument("--runs", type=int, default=None,
                   help="runs per timed scenario (default 3; 1 with --quick)")
    p.add_argument("--quick", action="store_true", help=f"only {','.join(QUICK_SCENARIOS)}, one run")
    p.add_argument("--only", help="comma-separated scenario ids: " + ",".join(ALL_SCENARIOS))
    p.add_argument("--cold", action="store_true",
                   help="build rust-modules/target-fast if absent instead of skipping the incremental rows")
    p.add_argument("--json", metavar="PATH", help="also write machine-readable results here")
    p.add_argument("--repo", default=str(ROOT), help=argparse.SUPPRESS)
    return p.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if os.environ.get("RELEASE"):
        print("build-bench: refused under RELEASE=1 -- it benchmarks the default-feature host build, "
              "not the shipping feature set.", file=sys.stderr)
        return 2
    repo = Path(args.repo).resolve()
    if args.only:
        scenarios = [s.strip() for s in args.only.split(",") if s.strip()]
        unknown = [s for s in scenarios if s not in ALL_SCENARIOS]
        if unknown:
            print(f"build-bench: unknown scenario(s): {', '.join(unknown)}", file=sys.stderr)
            return 2
    else:
        scenarios = list(QUICK_SCENARIOS if args.quick else ALL_SCENARIOS)
    runs = args.runs if args.runs is not None else (1 if args.quick else 3)
    if runs < 1:
        print("build-bench: --runs must be at least 1", file=sys.stderr)
        return 2

    for sig in (signal.SIGTERM, signal.SIGHUP):  # let the restoring `finally` blocks run
        signal.signal(sig, lambda n, _f: sys.exit(128 + n))

    edit_targets = sorted({EDIT_FILES[s] for s in scenarios if s in EDIT_FILES})
    try:
        cfg = read_make_config(repo)
        if cfg.get("RELEASE"):
            raise Refused("refused under RELEASE=1 -- it benchmarks the default-feature host build")
        EditGuard(repo).check_clean(edit_targets)
    except Refused as e:
        print(f"build-bench: {e}", file=sys.stderr)
        return 2

    cores = os.cpu_count() or 1
    via_make = bool(os.environ.get("PLX_BENCH_VIA_MAKE"))
    if not via_make:
        log("note: not run through `make build-bench`, so cargo sees a bare environment (a different "
            "fingerprint from make-driven builds; the first build will be a full one)")
    bench = Bench(repo, cfg, scenarios, runs, args.cold, cores)
    try:
        bench.execute()
    finally:
        shutil.rmtree(bench.cargo.runtime_dir, ignore_errors=True)
    bad = bench.guard.verify_clean(edit_targets)

    doc = {
        "schema": 1,
        "git_sha": git_out(repo, "rev-parse", "--short=12", "HEAD"),
        "tree_dirty": bool(git_out(repo, "status", "--porcelain", "--untracked-files=no")),
        "toolchain": bench.cargo.toolchain_version(),
        "host": host_info(cores),
        "runs": runs,
        "config": {k: cfg[k] for k in ("RUST_NIGHTLY", "RUST_TDIR", "RUST_TARGET", "RUST_FEATFLAGS",
                                       "TEST_FAST_TDIR")},
        "via_make": via_make,
        "sizes": bench.sizes,
        "warnings": bench.warnings,
        "failure": bench.failure,
        "restored_clean": not bad,
        "scenarios": scenario_docs(bench),
    }
    print(render_markdown(doc))
    if args.json:
        Path(args.json).write_text(json.dumps(doc, indent=2) + "\n")
    if bad:
        print(f"build-bench: ERROR: these edit targets differ from HEAD after the run: {', '.join(bad)}",
              file=sys.stderr)
        return 3
    return 1 if bench.failure else 0


if __name__ == "__main__":
    sys.exit(main())
