#!/usr/bin/env python3
"""The FPS tier against the SYNTHETIC mock server (`./tests/run.py --fps --mock`).

Why this exists (#395): Home must hold any number of rows, and the way to measure that is a Home
with a hundred and seventy of them, which only `tests/mock_pms.py --home-hubs N` can serve
repeatably. Until now `--fps` could not target it: it read `PMS_TOKEN` from `src/config.local.h`,
needed `tests/manifest.local.json` and resolved a plex.tv managed-user token. With `--mock` none of
those is touched; the identity is the one `tools/tv-session.sh up --guest --mock` boots
(`tools/mock-guest.py`, which refuses any server that is not the synthetic mock), and the harness
owns the mock's lifetime so a run is one command.

Everything here is pure or owns one subprocess, and none of it needs a television, so
`tests/test_harness.py` covers it on the host. The television-facing half (arming triggers, the
`make run`) stays in `run.py`; this module is what decides WHICH scenes run, WHAT the mock serves
for each, whether the log proves the app really talked to the mock, and how a paced key walk
through the landed Home rows is driven and cut out of the log for grading.

Scene fields (tests/manifest.json, `fps_scenes`):

  "mock": {}                    runnable under --mock with the mock's default library
  "mock": {"home_hubs": 170}    ... and the mock is started with `--home-hubs 170`
  "mock": {"only": true, ...}   runs ONLY under --mock (the scene is meaningless on a real library)
  "walk": {"key_gap_s": 0.33, "max_rows": 170, ...}
                                after the Home shelves land, press Down once per landed row at
                                that pace and then Up back, and grade the heartbeats of THAT window

A scene with no `mock` block is never run under --mock: it needs real library content, and guessing
which scenes do not is how a synthetic library grades the wrong thing.
"""
import importlib.util
import os
import re
import socket
import subprocess
import sys
import threading
import time
import urllib.request

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)

# `/identity`'s version on the synthetic mock, and the string the APP logs when it connects to it
# (`pms: server <slot> version=<v> ...`). The same literal `tools/mock-guest.py` checks before boot.
MOCK_SERVER_VERSION = "1.41.0.0000-synthetic"

# The only keys a walk may press. Never `ok`/`enter` (a Home card press resumes playback and writes
# a household's viewing record) and never `back` (on a root screen it leaves the app).
WALK_KEYS = ("down", "up")


# ---------------------------------------------------------------------------
# Scene selection
# ---------------------------------------------------------------------------
def scene_mock(scene):
    """The scene's `mock` block, or None when it does not declare one."""
    block = scene.get("mock")
    return block if isinstance(block, dict) else None


def mock_server_args(scene):
    """The extra `tests/mock_pms.py` arguments this scene's `mock` block asks for."""
    block = scene_mock(scene) or {}
    args = []
    if block.get("home_hubs") is not None:
        n = int(block["home_hubs"])
        if n < 0:
            raise ValueError(f"{scene.get('name')}: mock.home_hubs must not be negative")
        args += ["--home-hubs", str(n)]
    return args


def partition_mock(scenes, mock):
    """Split `scenes` into (runnable, [(name, reason), ...]) for this run's server.

    Under `--mock` a scene runs only if it declares a `mock` block. Without it, a scene that
    declares `mock.only` is skipped: it asks for a library only the mock can serve, and running it
    against a real one would grade a screen that was never built for it.
    """
    runnable, skipped = [], []
    for s in scenes:
        block = scene_mock(s)
        if mock and block is None:
            skipped.append((s["name"], "needs real library content (no `mock` block); "
                                       "not runnable against the synthetic mock"))
        elif not mock and block is not None and block.get("only"):
            skipped.append((s["name"], "mock-only scene: run with --mock"))
        else:
            runnable.append(s)
    return runnable, skipped


# ---------------------------------------------------------------------------
# The configured address, and the mock process
# ---------------------------------------------------------------------------
def configured_endpoint(config_path):
    """`(host, port)` from `PMS_HOST` / `PMS_PORT` in the gitignored `src/config.local.h`.

    Exits with a reason when either is absent: the binary under test was built against exactly this
    address, so there is nothing sensible to default to. Reads no token and never prints the file.
    """
    try:
        with open(config_path, encoding="utf-8") as fh:
            text = fh.read()
    except OSError as e:
        raise SystemExit(f"--mock needs {config_path} for PMS_HOST and PMS_PORT: {e}")
    host = re.search(r'^\s*#define\s+PMS_HOST\s+"([A-Za-z0-9.-]+)"\s*$', text, re.M)
    port = re.search(r"^\s*#define\s+PMS_PORT\s+(\d+)\s*$", text, re.M)
    if not host or not port or not 1 <= int(port[1]) <= 65535:
        raise SystemExit(f"--mock needs an explicit PMS_HOST and PMS_PORT in {config_path} "
                         f"(the address the deployed binary was built against)")
    return host[1], int(port[1])


def _mock_guest_module():
    spec = importlib.util.spec_from_file_location(
        "plx_mock_guest", os.path.join(REPO_ROOT, "tools", "mock-guest.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def resolve_mock_token(config_path):
    """The synthetic guest token, or SystemExit.

    The SAME function `tools/tv-session.sh up --guest --mock` runs: it fetches `/identity` and
    `/api/v2/user` from the configured address without following redirects and refuses anything
    that is not the synthetic mock. The token it returns is fabricated, never an account token.
    """
    try:
        return _mock_guest_module().resolve(config_path)
    except Exception as e:  # noqa: BLE001 — every failure is a refusal to boot
        raise SystemExit(f"refusing to run: {e}")


def _port_free(host, port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1)
        return s.connect_ex((host, port)) != 0


class MockPms:
    """One `tests/mock_pms.py` process on the configured address, for one scene.

    Refuses to start when something already answers on the address: an unrelated server there
    (a real PMS, or a mock started by hand with other arguments) must not be graded as if it were
    the scene's own.
    """

    def __init__(self, host, port, extra_args=(), log_path=None):
        self.host, self.port, self.extra = host, port, list(extra_args)
        self.proc = None
        self.log_path = log_path

    def argv(self):
        return [sys.executable, os.path.join(TESTS_DIR, "mock_pms.py"),
                "--host", self.host, "--port", str(self.port), *self.extra]

    def start(self, timeout=30):
        if not _port_free(self.host, self.port):
            raise SystemExit(f"refusing to run: something already answers on the configured PMS "
                             f"address ({self.host}:{self.port}); stop it so the harness can start "
                             f"the mock with this scene's arguments")
        out = open(self.log_path, "wb") if self.log_path else subprocess.DEVNULL
        self.proc = subprocess.Popen(self.argv(), stdout=out, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + timeout
        url = f"http://{self.host}:{self.port}/identity"
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(f"mock_pms exited with {self.proc.returncode} before serving "
                                 f"(args: {' '.join(self.extra) or 'defaults'})")
            try:
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                with opener.open(url, timeout=2):
                    return self
            except OSError:
                time.sleep(0.3)
        self.stop()
        raise SystemExit(f"mock_pms did not answer {url} within {timeout}s")

    def stop(self):
        p, self.proc = self.proc, None
        if p is None:
            return
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()

    def __enter__(self):
        return self.start()

    def __exit__(self, *exc):
        self.stop()
        return False


# ---------------------------------------------------------------------------
# What the event log proves
# ---------------------------------------------------------------------------
SERVER_RE = re.compile(r"^pms: server (\d+) version=(\S+)")
LANDED_RE = re.compile(r"^hubs: landed \S+ (\d+) items, (\d+) shelves")


def check_synthetic_identity(lines):
    """`(ok, reason)`: the app's own log must show it talked to the synthetic mock and nothing else.

    Fail closed: no `pms: server` line at all is a refusal (the app never reached a server, so there
    is nothing to call synthetic), and so is any server whose version is not the mock's. The check
    reads the APP's report of who answered, not the harness's belief about the address.
    """
    versions = [m.group(2) for m in (SERVER_RE.match(ln.strip()) for ln in lines) if m]
    if not versions:
        return False, "no `pms: server N version=` line: the app never identified a server"
    other = sorted({v for v in versions if v != MOCK_SERVER_VERSION})
    if other:
        return False, f"the app reported a non-synthetic server version ({', '.join(other)})"
    return True, f"app reported only the synthetic server ({MOCK_SERVER_VERSION})"


def landed(lines):
    """`(items, shelves)` from the LAST `hubs: landed` line, or None before Home has landed."""
    last = None
    for ln in lines:
        m = LANDED_RE.match(ln.strip())
        if m:
            last = (int(m.group(1)), int(m.group(2)))
    return last


def landed_count(lines):
    """How many `hubs: landed` lines the log has (Home refreshes show up as more than one)."""
    return sum(1 for ln in lines if LANDED_RE.match(ln.strip()))


# ---------------------------------------------------------------------------
# The paced walk
# ---------------------------------------------------------------------------
KEY_RE = re.compile(r"\bkey type=0x300\b")
HEARTBEAT_RE = re.compile(r"^loop=\d+ route=\w+")


def walk_secs_needed(scene):
    """Seconds one walk can take at most: land wait, settle, down + up at `key_gap_s`, the rest
    between the legs, and a closing tail. The manifest's `run_secs` must cover it (a unit test
    holds every walk scene to that), because the run is a fixed `sleep` on the TV."""
    w = scene["walk"]
    keys = 2 * int(w["max_rows"])
    return (scene.get("warmup_s", 5) + w.get("land_wait_s", 15) + w.get("settle_s", 3)
            + keys * float(w["key_gap_s"]) + w.get("rest_s", 4) + w.get("tail_s", 5))


def walk_plan(rows, walk):
    """The ordered key tokens for one walk: `rows` Downs, then `rows` Ups, capped at `max_rows`
    each way. Raises if a token outside WALK_KEYS could ever be produced."""
    n = min(int(rows), int(walk["max_rows"]))
    plan = ["down"] * n + ["up"] * n
    assert all(k in WALK_KEYS for k in plan)
    return plan


def walk_window(lines, tail_beats=2):
    """The slice of `lines` that is the walk: from the first key line to the last, plus
    `tail_beats` heartbeats after it so the settle of the final key is graded too. None when no
    key reached the app (the walk never ran, or the keys were lost)."""
    idx = [i for i, ln in enumerate(lines) if KEY_RE.search(ln)]
    if not idx:
        return None
    start, end = idx[0], idx[-1] + 1
    beats = 0
    while end < len(lines) and beats < tail_beats:
        if HEARTBEAT_RE.match(lines[end]):
            beats += 1
        end += 1
    return lines[start:end]


_KV = {k: re.compile(rf"\b{k}=(\d+(?:\.\d+)?)") for k in
       ("fps", "frame_n", "frame_gt16", "frame_gt33", "frame_gt50", "frame_gt100", "frame_max",
        "frame_p99")}


def frame_stats(window, route="home"):
    """Reported (not graded) frame statistics over the heartbeats of `window` on `route`: summed
    frame counts and drops, the worst single frame and p99, and the peak `fps=` (which also shows
    whether the panel is in its 60 or 50 Hz state)."""
    out = {"beats": 0, "frames": 0, "gt16": 0, "gt33": 0, "gt50": 0, "gt100": 0,
           "max_ms": 0.0, "p99_ms": 0.0, "peak_fps": 0}
    for ln in window or []:
        if not HEARTBEAT_RE.match(ln) or f"route={route}" not in ln or "frame_n=" not in ln:
            continue
        v = {k: float(rx.search(ln).group(1)) if rx.search(ln) else 0.0 for k, rx in _KV.items()}
        out["beats"] += 1
        out["frames"] += int(v["frame_n"])
        for k in ("gt16", "gt33", "gt50", "gt100"):
            out[k] += int(v[f"frame_{k}"])
        out["max_ms"] = max(out["max_ms"], v["frame_max"])
        out["p99_ms"] = max(out["p99_ms"], v["frame_p99"])
        out["peak_fps"] = max(out["peak_fps"], int(v["fps"]))
    return out


def describe_walk(rows_landed, keys_seen, stats, hub_lands):
    return (f" | walk: {rows_landed} row(s) landed, {keys_seen} key line(s), "
            f"{stats['beats']} heartbeat(s) in window, refresh peak fps={stats['peak_fps']}, "
            f"frames={stats['frames']} gt33={stats['gt33']} gt50={stats['gt50']} "
            f"gt100={stats['gt100']} worst={stats['max_ms']:.1f}ms p99max={stats['p99_ms']:.1f}ms "
            f"(recorded, not graded) | hubs landed {hub_lands}x")


class KeyWalk(threading.Thread):
    """Drives the paced Down-then-Up walk on the TV while `make run` is blocked in its `sleep`.

    One ssh carries every key: the pacing is done HERE because busybox `sleep` takes no fractions
    and an ssh per key costs hundreds of milliseconds of jitter. Each key is an `echo <name> >` into
    the app's own remote FIFO, the same in-app injection path `tools/tv-session.sh key` uses.
    The thread waits out the launch (the event log is deleted at launch, so an earlier scene's log
    must not be read as this one's), polls for the `hubs: landed` line, then walks all landed rows.
    """

    def __init__(self, ssh_argv, tv, eventlog, fifo, scene, launch_wait_s=10.0):
        super().__init__(daemon=True, name="key-walk")
        self.ssh_argv, self.tv, self.eventlog, self.fifo = ssh_argv, tv, eventlog, fifo
        self.scene, self.walk = scene, scene["walk"]
        self.launch_wait_s = launch_wait_s
        self.rows = None
        self.sent = 0
        self.error = None
        self.cancel = threading.Event()

    def _read_log(self):
        p = subprocess.run(self.ssh_argv(self.tv, f"cat {self.eventlog} 2>/dev/null"),
                           capture_output=True, text=True, timeout=30)
        return p.stdout.splitlines()

    def run(self):
        try:
            self._run()
        except Exception as e:  # noqa: BLE001 — surfaced by the grader, never raised in a thread
            self.error = str(e)

    def _run(self):
        if self.cancel.wait(self.launch_wait_s):
            return
        deadline = time.monotonic() + float(self.walk.get("land_wait_s", 15)) + \
            float(self.scene.get("warmup_s", 5))
        found = None
        while time.monotonic() < deadline and not self.cancel.is_set():
            found = landed(self._read_log())
            if found:
                break
            self.cancel.wait(2.0)
        if not found:
            self.error = "Home never logged `hubs: landed`, so there were no rows to walk"
            return
        self.rows = found[1]
        if self.cancel.wait(float(self.walk.get("settle_s", 3))):
            return
        plan = walk_plan(self.rows, self.walk)
        gap = float(self.walk["key_gap_s"])
        rest = float(self.walk.get("rest_s", 4))
        proc = subprocess.Popen(self.ssh_argv(self.tv, "sh"), stdin=subprocess.PIPE, text=True,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            half = len(plan) // 2
            nxt = time.monotonic()
            for i, key in enumerate(plan):
                if self.cancel.is_set():
                    break
                proc.stdin.write(f"echo {key} > {self.fifo}\n")
                proc.stdin.flush()
                self.sent += 1
                nxt += gap
                if i + 1 == half:
                    nxt += rest
                delay = nxt - time.monotonic()
                if delay > 0:
                    self.cancel.wait(delay)
        finally:
            try:
                proc.stdin.close()
            except OSError:
                pass
            try:
                proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                proc.kill()

