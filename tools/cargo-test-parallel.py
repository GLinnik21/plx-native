#!/usr/bin/env python3
"""Run the per-crate `cargo test --lib` binaries CONCURRENTLY and keep `make check` honest about it.

    tools/cargo-test-parallel.py [--jobs N] [--filter NAME ...] [--expect N] -- cargo [+tc] test --lib -p a -p b ...

Why this exists. The workspace is 14 crates, so `cargo test --lib -p ... -p ...` builds 14 test
binaries and RUNS THEM ONE AFTER ANOTHER (cargo has no parallel test execution). Before the split
one binary held every test, so a slow test overlapped with 5000 others; now each binary's slowest
test (or its longest chain of `testlock::serial()` tests) is a wall-clock tail nothing overlaps
with. Measured 2026-10-05: the sequential per-binary times sum to the suite's wall time, and
`plx_media` alone was 28.7 s of 78.8 s. Running the binaries side by side turns the sum into a
max. See docs/agent-reference.md ("Unit suite").

What it does, in order:

1. Runs the cargo command you give it with `--no-run --message-format=json-render-diagnostics`
   inserted right after the `test` subcommand. Compiler diagnostics and progress go to this
   process's stderr exactly as cargo prints them; the JSON stream is read here. A failed build
   exits with cargo's own status. EVERYTHING after `--` is the cargo command, so the packages,
   feature flags, toolchain (`+nightly`) and environment (CARGO_INCREMENTAL, CARGO_TARGET_DIR,
   telemetry words) are the caller's, unchanged.
2. Collects every test executable cargo reports. If the command named `-p` packages, it must
   have produced exactly one executable per package -- a package that silently stopped
   producing a test binary would otherwise shrink the gate without a failure -- and at least one
   executable must exist.
3. Runs them with a bounded number of jobs (`--jobs`, default `PLX_TEST_JOBS` or min(6, CPUs)), largest
   binary first. Each runs the way cargo runs it: cwd is the package directory and the
   CARGO_MANIFEST_DIR / CARGO_PKG_* / library-path environment is set. Each also gets its own
   PLXNATIVE_RUNTIME_DIR (a subdirectory of the caller's, named for the package; only the hostsim
   build reads it) and its own
   RUST_TEST_THREADS share, so the binaries cannot share the runtime root's files and the
   machine is not oversubscribed jobs-fold (timing tests are sensitive to that). Each runs with
   NO_PROXY=* so the curl tests' loopback servers are never handed to a caller's HTTPS_PROXY.
   Each also gets its own EMPTY `TMPDIR` (a `tmp` directory beside that runtime subdirectory),
   and a binary that exits leaving anything in it FAILS the run, naming the leftovers: a test
   fixture that creates a `$TMPDIR/<name>-<pid>` and never removes it used to pile up tens of
   thousands of directories in the developer's temp dir, because the only removal was a later
   process reusing the pid.
4. Prints each binary's output WHOLE, under a cargo-style `Running` line, in the order they
   finish -- never interleaved -- so every binary's literal `test result:` line is in the log.
   It does not stop at the first failing binary (cargo would): every binary runs, every failure
   is printed, then the exit status is 101 if any binary failed, else 0.

`--filter NAME` (repeatable) is the TESTNAME filter cargo takes positionally; it is passed to each
binary. A binary with no match prints `0 passed ... N filtered out` and succeeds, as under cargo.
Arguments after a SECOND `--` inside the cargo command are libtest flags and are passed through.

SIGINT/SIGTERM/SIGHUP stop every running test binary's process group before exiting.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor

HEARTBEAT = 60.0
EX_TESTFAIL = 101  # cargo's own status for "a test failed"


def default_jobs() -> int:
    """6 on a 10-core Mac (measured: 14 s wall, the same as running all 14 at once; 4 gave 18 s)."""
    return max(2, min(6, os.cpu_count() or 1))


def split_cargo(argv: list[str]) -> tuple[list[str], list[str]]:
    """(cargo args with the libtest tail removed, the libtest tail)."""
    if "--" in argv:
        i = argv.index("--")
        return argv[:i], argv[i + 1:]
    return argv, []


def with_json_no_run(argv: list[str]) -> list[str]:
    if "test" not in argv:
        raise SystemExit("cargo-test-parallel: the cargo command has no `test` subcommand")
    if any(a.startswith("--message-format") or a == "--no-run" for a in argv):
        raise SystemExit("cargo-test-parallel: the cargo command already sets --no-run/--message-format")
    # Appended, so the command still reads `cargo +tc test --lib -p a -p b ...` up to the flags
    # that make it a build; a positional TESTNAME is not part of it (see --filter).
    return argv + ["--no-run", "--message-format=json-render-diagnostics"]


def requested_packages(argv: list[str]) -> list[str]:
    names, i = [], 0
    while i < len(argv):
        a = argv[i]
        if a in ("-p", "--package") and i + 1 < len(argv):
            names.append(argv[i + 1]); i += 2; continue
        if a.startswith("--package="):
            names.append(a.split("=", 1)[1])
        i += 1
    return names


def collect(stream) -> tuple[list[dict], dict[str, list[str]]]:
    """Test executables, and per-package native link paths from the build scripts."""
    tests, link_paths = [], {}
    for raw in stream:
        try:
            msg = json.loads(raw)
        except ValueError:
            continue
        reason = msg.get("reason")
        if reason == "compiler-artifact" and msg.get("executable") and msg.get("profile", {}).get("test"):
            tests.append(msg)
        elif reason == "build-script-executed":
            link_paths[msg["package_id"]] = msg.get("linked_paths", [])
    return tests, link_paths


def libpath_var() -> str:
    return "DYLD_FALLBACK_LIBRARY_PATH" if sys.platform == "darwin" else "LD_LIBRARY_PATH"


def package_name(msg: dict) -> str:
    return msg["target"]["name"]


class Run:
    def __init__(self, msg: dict, link_paths: list[str], runtime_root: str | None, threads: int | None,
                 filters: list[str], libtest: list[str]):
        self.msg = msg
        self.exe = msg["executable"]
        self.name = package_name(msg)
        self.manifest_dir = os.path.dirname(msg["manifest_path"])
        self.cmd = [self.exe, *filters, *libtest]
        self.env = dict(os.environ)
        self.env["CARGO_MANIFEST_DIR"] = self.manifest_dir
        self.env["CARGO_CRATE_NAME"] = self.name
        self.env["CARGO_PRIMARY_PACKAGE"] = "1"
        pkg = msg["package_id"]
        # `path+file:///…/rust-modules/base#plx_base@0.0.0` (or the old `plx_base 0.0.0 (path+…)`).
        if "#" in pkg:
            tail = pkg.rsplit("#", 1)[1]
            if "@" in tail:
                self.env["CARGO_PKG_NAME"], self.env["CARGO_PKG_VERSION"] = tail.rsplit("@", 1)
        extra = [os.path.dirname(self.exe), os.path.dirname(os.path.dirname(self.exe))]
        extra += [p.split("=", 1)[1] if "=" in p else p for p in link_paths]
        var = libpath_var()
        old = self.env.get(var)
        self.env[var] = os.pathsep.join([*extra, *([old] if old else [])])
        self.tmpdir: str | None = None
        self.leaked: list[str] = []
        if runtime_root is not None:
            sub = os.path.join(runtime_root, self.name)
            os.makedirs(sub, exist_ok=True)
            self.env["PLXNATIVE_RUNTIME_DIR"] = sub
            self.tmpdir = os.path.join(sub, "tmp")
            os.makedirs(self.tmpdir, exist_ok=True)
            # The user's own primary group, as a real per-user temp directory has: a directory made
            # under a parent of another group hands that group to every file created inside it, and
            # the storage-diagnostics tests refuse files that are not ours.
            try:
                os.chown(self.tmpdir, -1, os.getegid())
            except OSError:
                pass
            os.chmod(self.tmpdir, 0o700)
            self.env["TMPDIR"] = self.tmpdir
        # No proxy for the test binaries, whatever the caller's environment carries: the curl tests
        # aim fake names (`*.plex.direct`, `.invalid`) at loopback servers of their own, and libcurl
        # would hand them to an HTTPS_PROXY instead (a cloud agent sandbox sets one; ~20 TLS and
        # redirect tests then fail with CURLE_SSL_CONNECT_ERROR). The TV has no proxy. Only the
        # binaries get this; cargo's own crate downloads above still use the caller's proxy.
        self.env["NO_PROXY"] = self.env["no_proxy"] = "*"
        if threads and "RUST_TEST_THREADS" not in os.environ:
            self.env["RUST_TEST_THREADS"] = str(threads)
        self.proc: subprocess.Popen | None = None
        self.started = 0.0
        self.elapsed = 0.0
        self.code: int | None = None
        self.output = b""

    def size(self) -> int:
        try:
            return os.path.getsize(self.exe)
        except OSError:
            return 0

    def sweep_tmpdir(self) -> None:
        """Whatever the binary left in its private `TMPDIR` is a leak: record it and remove it."""
        if not self.tmpdir:
            return
        try:
            self.leaked = sorted(os.listdir(self.tmpdir))
        except OSError:
            return
        for name in self.leaked:
            # Read-only subdirectories (a fixture's "unwritable" candidate) block a plain rm -r.
            subprocess.run(["chmod", "-R", "u+rwx", os.path.join(self.tmpdir, name)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            subprocess.run(["rm", "-rf", os.path.join(self.tmpdir, name)])


def main() -> int:
    ap = argparse.ArgumentParser(usage="%(prog)s [--jobs N] [--filter NAME ...] -- cargo ... test ...",
                                 description=__doc__.split("\n\n")[0])
    ap.add_argument("--jobs", type=int, default=int(os.environ.get("PLX_TEST_JOBS") or default_jobs()),
                    help="binaries run at once (default PLX_TEST_JOBS, else min(6, CPUs), at least 2)")
    ap.add_argument("--filter", action="append", default=[])
    ap.add_argument("--threads", type=int, default=int(os.environ.get("PLX_TEST_THREADS") or 0),
                    help="RUST_TEST_THREADS for each binary (0: an even share of the CPUs, at least 2)")
    ap.add_argument("cargo", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    cargo = args.cargo[1:] if args.cargo[:1] == ["--"] else args.cargo
    if not cargo or args.jobs < 1:
        ap.error("need a cargo command after `--` and --jobs >= 1")
    build_args, libtest = split_cargo(cargo)

    build = subprocess.run(with_json_no_run(build_args), stdout=subprocess.PIPE, text=True)
    if build.returncode != 0:
        return build.returncode
    tests, link_paths = collect(build.stdout.splitlines())

    wanted = requested_packages(build_args)
    if not tests:
        print("cargo-test-parallel: cargo reported no test executable -- refusing to call that a pass",
              file=sys.stderr)
        return 1
    if wanted and len(tests) != len(wanted):
        got = sorted(package_name(t) for t in tests)
        print(f"cargo-test-parallel: asked for {len(wanted)} packages ({', '.join(sorted(wanted))}) but "
              f"cargo built {len(tests)} test executables ({', '.join(got)})", file=sys.stderr)
        return 1

    ncpu = os.cpu_count() or 1
    jobs = min(args.jobs, len(tests))
    threads = args.threads or (max(2, -(-ncpu // jobs)) if jobs > 1 else None)
    runtime_root = os.environ.get("PLXNATIVE_RUNTIME_DIR")
    scratch = None
    if runtime_root is None:
        scratch = tempfile.mkdtemp(prefix="plxnative-check.")
        runtime_root = scratch
    runs = [Run(t, link_paths.get(t["package_id"], []), runtime_root, threads, args.filter, libtest)
            for t in tests]
    runs.sort(key=lambda r: -r.size())  # largest binary first: the long poles start at t=0

    print_lock = threading.Lock()
    live: set[Run] = set()
    stopping = threading.Event()
    t0 = time.monotonic()

    def run_one(r: Run) -> None:
        if stopping.is_set():
            return
        r.started = time.monotonic()
        try:
            r.proc = subprocess.Popen(r.cmd, cwd=r.manifest_dir, env=r.env, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      start_new_session=True)
        except OSError as error:
            r.code, r.output = 127, f"cargo-test-parallel: cannot start {r.exe}: {error}\n".encode()
            r.elapsed = 0.0
        else:
            with print_lock:
                live.add(r)
            r.output, _ = r.proc.communicate()
            r.code = r.proc.returncode
            r.elapsed = time.monotonic() - r.started
            r.sweep_tmpdir()
            if r.leaked:
                r.output += ("\ncargo-test-parallel: this binary left " + str(len(r.leaked)) +
                             " entr" + ("y" if len(r.leaked) == 1 else "ies") +
                             " in its TMPDIR after it exited (a fixture that creates scratch space "
                             "and never removes it; plx_base::testscratch removes per-process "
                             "directories at exit): " + ", ".join(r.leaked[:8]) + "\n").encode()
                if r.code == 0:
                    r.code = 1
        with print_lock:
            live.discard(r)
            rel = os.path.relpath(r.exe)
            sys.stdout.write(f"     Running unittests ({rel}) [{r.name}, {r.elapsed:.1f}s]\n")
            sys.stdout.write(r.output.decode(errors="replace"))
            if r.output and not r.output.endswith(b"\n"):
                sys.stdout.write("\n")
            sys.stdout.flush()

    def stop(signum, _frame):
        stopping.set()
        for r in list(live):
            if r.proc and r.proc.poll() is None:
                try:
                    os.killpg(r.proc.pid, signal.SIGTERM)
                except OSError:
                    pass
        # Fall through: the pool drains (queued runs return immediately) and main reports.
        stop.signum = signum

    stop.signum = 0
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, stop)

    done = threading.Event()

    def heartbeat() -> None:
        while not done.wait(HEARTBEAT):
            with print_lock:
                names = ", ".join(sorted(r.name for r in live))
            print(f"cargo-test-parallel: {time.monotonic() - t0:.0f}s, still running: {names or '(none)'}",
                  file=sys.stderr, flush=True)

    threading.Thread(target=heartbeat, daemon=True).start()
    try:
        with ThreadPoolExecutor(max_workers=jobs) as pool:
            list(pool.map(run_one, runs))
    finally:
        done.set()
        if scratch:
            subprocess.run(["rm", "-rf", scratch])
    if stop.signum:
        print(f"cargo-test-parallel: interrupted by signal {stop.signum}", file=sys.stderr)
        return 128 + stop.signum

    failed = [r for r in runs if r.code != 0]
    wall = time.monotonic() - t0
    print(f"\ncargo-test-parallel: {len(runs)} test binaries, {jobs} at a time, {wall:.1f}s wall "
          f"(sum of binaries {sum(r.elapsed for r in runs):.1f}s)")
    if failed:
        for r in failed:
            print(f"cargo-test-parallel: FAILED {r.name} (exit {r.code})")
        print("error: test failed, to rerun pass `--lib` for the failing package(s) with the same flags")
        return EX_TESTFAIL
    return 0


if __name__ == "__main__":
    sys.exit(main())
