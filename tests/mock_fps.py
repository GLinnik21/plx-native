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
  "mock": {"section_hubs": 170, "section_hubs_linked": 40}
                                ... `--section-hubs 170 --section-hubs-linked 40` (#412): the
                                Library's `/hubs/sections/<id>` answers 170 hubs, 40 of them
                                promoted collections
  "mock": {"movies": 240}       ... `--movies 240`: a bigger synthetic library (collections grow with it)
  "mock": {"rk": 50002}         a scene that names a library `item` on a real server opens THIS
                                ratingKey of the synthetic library instead (`$rk` in its triggers);
                                an `item` scene with no `mock.rk` is not runnable under --mock
  "mock": {"triggers": {"plxnative-search": "sb"}}
                                trigger values that replace the scene's own under --mock (a query
                                that the synthetic titles, all `s` + hex, can actually match)
  "mock": {"only": true, ...}   runs ONLY under --mock (the scene is meaningless on a real library)
  "walk": {"key_gap_s": 0.33, "max_rows": 170, ...}
                                after the Home shelves land, press Down once per landed row at
                                that pace and then Up back, and grade the heartbeats of THAT window
  "walk": {"landed": "library", "extra_rows": 6, "fingerprint_region": "grid", ...}
                                the same walk over the Library: sized from the seated section's
                                `libhubs: section N landed M shelves` line, a second Down per
                                linked shelf, `extra_rows` more to reach the grid, and a
                                `focus route=... region=grid` fingerprint required in the window
  "walk": {"min_landed": 150, ...}
                                the scene FAILS when fewer shelves (rows) than that landed, so a
                                cap that shortens the surface cannot pass on the shorter walk

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
    if block.get("movies") is not None:
        n = int(block["movies"])
        if not 0 <= n <= 1000:
            raise ValueError(f"{scene.get('name')}: mock.movies must be between 0 and 1000")
        args += ["--movies", str(n)]
    if block.get("home_hubs") is not None:
        n = int(block["home_hubs"])
        if n < 0:
            raise ValueError(f"{scene.get('name')}: mock.home_hubs must not be negative")
        args += ["--home-hubs", str(n)]
    if block.get("section_hubs") is not None:
        n = int(block["section_hubs"])
        linked = int(block.get("section_hubs_linked") or 0)
        if n < 0 or linked < 0:
            raise ValueError(f"{scene.get('name')}: mock.section_hubs[_linked] must not be negative")
        if linked > n:
            raise ValueError(f"{scene.get('name')}: mock.section_hubs_linked exceeds section_hubs")
        args += ["--section-hubs", str(n)]
        if linked:
            args += ["--section-hubs-linked", str(linked)]
    elif block.get("section_hubs_linked"):
        raise ValueError(f"{scene.get('name')}: mock.section_hubs_linked needs mock.section_hubs")
    return args


def mock_scene(scene):
    """The scene as it runs under --mock: `mock.rk` becomes its `rk` and `mock.triggers` replace
    the same-named triggers. A copy; the manifest's own scene is untouched."""
    block = scene_mock(scene) or {}
    out = dict(scene)
    if block.get("rk") is not None:
        out["rk"] = int(block["rk"])
    if block.get("triggers"):
        out["triggers"] = {**scene.get("triggers", {}), **block["triggers"]}
    return out


def partition_mock(scenes, mock):
    """Split `scenes` into (runnable, [(name, reason), ...]) for this run's server.

    Under `--mock` a scene runs only if it declares a `mock` block. Without it, a scene that
    declares `mock.only` is skipped: it asks for a library only the mock can serve, and running it
    against a real one would grade a screen that was never built for it.
    """
    runnable, skipped = [], []
    for s in scenes:
        block = scene_mock(s)
        if mock and block is not None and s.get("item") and block.get("rk") is None:
            skipped.append((s["name"], "names a library `item` and has no `mock.rk`: "
                                       "not runnable against the synthetic mock"))
        elif mock and block is None:
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

    def start(self, timeout=120):
        # A hang guard, not a budget: the poll below returns the moment the mock answers, and 30 s was
        # not enough for a python interpreter to start on a 3-core runner that was also compiling
        # (build-bench run 37703389591: "mock_pms did not answer ... within 30s").
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
# The Library's own landing line (`browse/section_hubs.rs`): `section` is the section's INDEX in the
# directory (the mock lists Movies first, then Shows), not its Plex key.
LIBHUBS_RE = re.compile(r"^libhubs: section (\d+) landed (\d+) shelves")


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


def library_section_index(scene):
    """The directory index of the section `plxnative-library` seats: content `1` is the preferred
    Shows library (index 1), anything else (a bare flag, `0`, ...) the preferred Movies library
    (index 0) — `dev/scenarios.rs`, `library` trigger. The mock lists Movies before Shows."""
    content = (scene.get("triggers") or {}).get("plxnative-library")
    return 1 if str(content).strip() == "1" else 0


def landed_library(lines, section):
    """The shelf count from the LAST `libhubs: section <section> landed` line, or None.

    The last line OF THAT SECTION, not of the log: the Library fetches the other section's hubs
    too, and a Shows landing after the Movies one must not stand in for it. The last line (rather
    than the first) is right because a refresh re-lands the section and the app shows the newest."""
    last = None
    for ln in lines:
        m = LIBHUBS_RE.match(ln.strip())
        if m and int(m.group(1)) == section:
            last = int(m.group(2))
    return last


def landed_library_count(lines, section):
    """How many times the Library landed `section` (refreshes show up as more than one)."""
    return sum(1 for ln in lines
               if (m := LIBHUBS_RE.match(ln.strip())) and int(m.group(1)) == section)


def landed_shelves(scene, lines):
    """The shelf count the scene's walk is sized from: the seated Library section's landing when
    `walk.landed` is `"library"`, otherwise Home's. None before it has landed."""
    if (scene.get("walk") or {}).get("landed") == "library":
        return landed_library(lines, library_section_index(scene))
    found = landed(lines)
    return found[1] if found else None


def landed_lands(scene, lines):
    """How many times the scene's walk surface landed (see `landed_shelves`)."""
    if (scene.get("walk") or {}).get("landed") == "library":
        return landed_library_count(lines, library_section_index(scene))
    return landed_count(lines)


# ---------------------------------------------------------------------------
# The paced walk
# ---------------------------------------------------------------------------
# What KeyWalk sleeps before it first reads the log (the log is deleted at launch). It is part of
# the run, so `walk_secs_needed` counts it.
LAUNCH_WAIT_S = 10.0


def linked_landed(scene, shelves):
    """How many of the `shelves` that landed are promoted collections (two Down stops each).

    The harness cannot read that from the app's log, so it is derived from what the mock was
    told to serve: the library's own two hubs (Continue Watching, Recently Added) come first, then
    the padding hubs in order with `section_hubs_linked` of them spread evenly (mock_pms
    `section_pad_hubs`), so the first k padding hubs hold `k * M // padded` linked ones. An
    estimate of +-1 at the boundary (an empty Continue Watching hub lands no shelf) only shifts the
    extra presses and is absorbed by `extra_rows`; the walker and the grader share this one
    function, so they cannot disagree about it."""
    block = scene_mock(scene) or {}
    total, linked = int(block.get("section_hubs") or 0), int(block.get("section_hubs_linked") or 0)
    padded = total - 2
    if padded <= 0 or linked <= 0:
        return 0
    return min(max(int(shelves) - 2, 0), padded) * linked // padded


def walk_downs(scene, shelves):
    """The Down presses a walk of `scene` makes for `shelves` landed shelves (before the
    `max_rows` cap `walk_plan` applies). Home: one per row. Library (`walk.landed: "library"`):
    one per shelf, a SECOND for each linked shelf (its heading is a stop of its own, then its
    row), and `extra_rows` more to cross the toolbar into the grid."""
    w = scene["walk"]
    if w.get("landed") != "library":
        return int(shelves)
    return int(shelves) + linked_landed(scene, shelves) + int(w.get("extra_rows", 0))


def is_burst(scene):
    """Whether the scene's `walk` is a burst walk (`walk.burst`): short runs of keys with rests
    between them, instead of one continuous Down leg and one Up leg."""
    return bool((scene.get("walk") or {}).get("burst"))


def burst_shape(walk):
    """`(burst, bursts, key_s, rest_s)` of a burst walk: `burst` keys `key_ms` apart, then
    `rest_ms` of nothing, `bursts` times in each direction. Refuses a shape that is not a walk."""
    burst, bursts = int(walk["burst"]), int(walk["bursts"])
    key_s, rest_s = float(walk["key_ms"]) / 1000.0, float(walk["rest_ms"]) / 1000.0
    if burst < 1 or bursts < 1 or key_s <= 0 or rest_s < 0:
        raise ValueError(f"walk burst={burst} bursts={bursts} key_ms/rest_ms={key_s * 1000}/"
                         f"{rest_s * 1000} is not a walk")
    return burst, bursts, key_s, rest_s


def walk_secs_needed(scene):
    """Seconds one walk can take at most: the launch wait, land wait, settle, down + up at
    `key_gap_s`, the rest between the legs, and a closing tail. The manifest's `run_secs` must
    cover it (a unit test holds every walk scene to that), because the run is a fixed `sleep` on
    the TV. A burst walk (`walk.burst`) spends `max_rows` keys `key_ms` apart reaching the grid,
    a rest, then `bursts` bursts down and as many back up, each followed by its rest."""
    w = scene["walk"]
    if is_burst(scene):
        burst, bursts, key_s, rest_s = burst_shape(w)
        walking = (int(w["max_rows"]) * key_s + rest_s
                   + 2 * bursts * (burst * key_s + rest_s))
    else:
        walking = 2 * int(w["max_rows"]) * float(w["key_gap_s"]) + w.get("rest_s", 4)
    return (LAUNCH_WAIT_S + scene.get("warmup_s", 5) + w.get("land_wait_s", 15)
            + w.get("settle_s", 3) + walking + w.get("tail_s", 5))


def grid_fingerprint_seen(window, route, region):
    """Whether `window` holds a `focus route=<route> ... region=<region>` fingerprint
    (`focusprobe`, armed by `plxnative-focus`; logged on change, so one line is a focus MOVE)."""
    rx = re.compile(rf"\bfocus route={re.escape(route)}\b.*\bregion={re.escape(region)}\b")
    return any(rx.search(ln) for ln in window or [])


def deepest_shelf_row(window, route):
    """The largest `row=` among the window's `focus route=<route> ... region=shelf row=<R>`
    fingerprints (`focusprobe`, armed by `plxnative-focus`), or None when there is none. This is
    the SCREEN's evidence of how deep the walk got, as opposed to the data layer's landed count."""
    rx = re.compile(rf"\bfocus route={re.escape(route)}\b.*\bregion=shelf row=(\d+)\b")
    rows = [int(m.group(1)) for ln in window or [] if (m := rx.search(ln))]
    return max(rows) if rows else None


KEY_RE = re.compile(r"\bkey type=0x300\b")
HEARTBEAT_RE = re.compile(r"^loop=\d+ route=\w+")


def walk_plan(rows, walk):
    """The ordered key tokens for one walk: `rows` Downs, then `rows` Ups, capped at `max_rows`
    each way. Raises if a token outside WALK_KEYS could ever be produced."""
    n = min(int(rows), int(walk["max_rows"]))
    plan = ["down"] * n + ["up"] * n
    assert all(k in WALK_KEYS for k in plan)
    return plan


def burst_plan(rows, walk):
    """The ordered `(key, seconds_until_the_next_key)` pairs of a burst walk over `rows` landed
    shelves: the Downs that cross the shelves and the toolbar into the grid (`walk_downs`'s share
    is the caller's `rows`, capped at `max_rows`) at `key_ms`, a rest, then `bursts` bursts of
    `burst` Downs, then the same bursts of Ups. Every burst ends in `rest_ms`; the keys inside a
    burst are `key_ms` apart. Raises if a token outside WALK_KEYS could ever be produced."""
    burst, bursts, key_s, rest_s = burst_shape(walk)
    n = min(int(rows), int(walk["max_rows"]))
    steps = [("down", key_s)] * n
    if steps:
        steps[-1] = ("down", rest_s)
    for key in ("down", "up"):
        for _ in range(bursts):
            steps += [(key, key_s)] * (burst - 1) + [(key, rest_s)]
    assert all(k in WALK_KEYS for k, _ in steps)
    return steps


def walk_steps(scene, rows):
    """`(key, seconds_until_the_next_key)` for every key the walk of `scene` presses over `rows`
    landed shelves/rows. A plain walk is `walk_plan` at `key_gap_s` with `rest_s` added after the
    last Down; a burst walk is `burst_plan`. KeyWalk paces itself from this and nothing else."""
    w = scene["walk"]
    if is_burst(scene):
        return burst_plan(walk_downs(scene, rows), w)
    plan = walk_plan(walk_downs(scene, rows), w)
    gap, rest = float(w["key_gap_s"]), float(w.get("rest_s", 4))
    half = len(plan) // 2
    return [(key, gap + (rest if i + 1 == half else 0.0)) for i, key in enumerate(plan)]


def walk_expected_keys(scene, rows):
    """How many keys a complete walk of `scene` sends over `rows` landed shelves/rows, 0 when
    nothing landed. The grader's count and the walker's plan are the same list, so they cannot
    disagree."""
    return len(walk_steps(scene, rows)) if rows else 0


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


def describe_walk(rows_landed, keys_seen, stats, hub_lands, unit="row"):
    """`unit` is what landed: Home's `row`s, or the Library's `shelf`s (`walk.landed: "library"`)."""
    return (f" | walk: {rows_landed} {unit}(s) landed, {keys_seen} key line(s), "
            f"{stats['beats']} heartbeat(s) in window, refresh peak fps={stats['peak_fps']}, "
            f"frames={stats['frames']} gt33={stats['gt33']} gt50={stats['gt50']} "
            f"gt100={stats['gt100']} worst={stats['max_ms']:.1f}ms p99max={stats['p99_ms']:.1f}ms "
            f"(recorded, not graded) | {'shelves' if unit == 'shelf' else 'hubs'} landed {hub_lands}x")


# ---------------------------------------------------------------------------
# Placeholder accounting (a burst walk's instrument)
# ---------------------------------------------------------------------------
# `phcount: frames=F ph_frames=P draws=D ph=N`, one line per second while `plxnative-phcount` is
# armed (`card_motion_metrics::interval_line`): presented card frames, those that showed at least
# one card without a texture, card draws and the draws without a texture (the placeholder
# tile-frames). It is its own event-log line, not a heartbeat field.
PHCOUNT_RE = re.compile(r"^phcount: frames=(\d+) ph_frames=(\d+) draws=(\d+) ph=(\d+)\s*$")


def phcount_samples(window):
    """The `phcount:` lines of `window` as `(keys_before, frames, ph_frames, draws, ph)`, where
    `keys_before` is how many key lines precede the sample in the window (its place in the walk)."""
    out, keys = [], 0
    for ln in window or []:
        if KEY_RE.search(ln):
            keys += 1
            continue
        m = PHCOUNT_RE.match(ln.strip())
        if m:
            out.append((keys, *(int(g) for g in m.groups())))
    return out


def placeholder_tally(window, walk, prelude_keys):
    """Placeholder tile-frames of a burst walk's `window`, or None when no `phcount:` line is in it
    (the instrument was not armed, or the build has no devtriggers: not the same as zero).

    `prelude_keys` is how many keys cross into the grid before the first burst. A sample belongs
    to the burst of the last key before it, so a burst owns the rest that follows it (that is the
    time a warm-while-at-rest strategy works in); its second-granular lines make a per-burst count
    good to about a line either side of a boundary. Bursts `0..bursts-1` are the Down leg and
    `bursts..2*bursts-1` the Up leg."""
    samples = phcount_samples(window)
    if not samples:
        return None
    burst, bursts, _, _ = burst_shape(walk)
    per = [0] * (2 * bursts)
    prelude = 0
    for keys, _frames, _ph_frames, _draws, ph in samples:
        if keys <= prelude_keys:
            prelude += ph
        else:
            per[min((keys - prelude_keys - 1) // burst, 2 * bursts - 1)] += ph
    return {"lines": len(samples), "frames": sum(x[1] for x in samples),
            "ph_frames": sum(x[2] for x in samples), "draws": sum(x[3] for x in samples),
            "ph": sum(x[4] for x in samples), "prelude": prelude, "per_burst": per,
            "bursts": bursts}


_FPS_RE = re.compile(r"\bfps=(\d+)")
_PUP_RE = {k: re.compile(rf"\b{k}=(\d+)") for k in ("pupf", "pupf_ge22", "pup")}


def describe_burst(window, route, walk, prelude_keys):
    """The printed (never graded) half of a burst walk: placeholder tile-frames in total and per
    burst, the median `fps=` over the window's heartbeats, and the poster-upload tally
    (`pupf=`/`pupf_ge22=`/`pup=`) summed over them."""
    beats = [ln for ln in window or [] if HEARTBEAT_RE.match(ln) and f"route={route}" in ln]
    fps = sorted(int(m.group(1)) for ln in beats if (m := _FPS_RE.search(ln)))
    med = f"{fps[len(fps) // 2]} (min {fps[0]}, max {fps[-1]}, n={len(fps)})" if fps else "n/a"
    pup = {k: sum(int(m.group(1)) for ln in beats if (m := rx.search(ln)))
           for k, rx in _PUP_RE.items()}
    seen = any("pupf=" in ln for ln in beats)
    tally = placeholder_tally(window, walk, prelude_keys)
    if tally is None:
        ph = ("placeholders: n/a (no `phcount:` line in the walk window: is plxnative-phcount "
              "armed, and is this a devtriggers build?)")
    else:
        b = tally["bursts"]
        ph = (f"placeholders: ph={tally['ph']} tile-frames in {tally['lines']} s "
              f"({tally['ph_frames']} of {tally['frames']} card frames showed one; "
              f"prelude ph={tally['prelude']}, bursts ph={sum(tally['per_burst'])}) | "
              f"per burst down {tally['per_burst'][:b]} up {tally['per_burst'][b:]}")
    ups = (f"pupf={pup['pupf']} pupf_ge22={pup['pupf_ge22']} pup={pup['pup']}" if seen
           else "pupf/pup n/a")
    return f" | burst: {ph} | median fps {med} | {ups} (recorded, not graded)"


class KeyWalk(threading.Thread):
    """Drives the paced Down-then-Up walk on the TV while `make run` is blocked in its `sleep`.

    One ssh carries every key: the pacing is done HERE because busybox `sleep` takes no fractions
    and an ssh per key costs hundreds of milliseconds of jitter. Each key is an `echo <name> >` into
    the app's own remote FIFO, the same in-app injection path `tools/tv-session.sh key` uses.
    The thread waits out the launch (the event log is deleted at launch, so an earlier scene's log
    must not be read as this one's), polls for the scene's landing line (Home's `hubs: landed`, or
    the seated section's `libhubs: section N landed` under `walk.landed: "library"`), then walks
    every landed row (`walk_downs`).
    """

    def __init__(self, ssh_argv, tv, eventlog, fifo, scene, launch_wait_s=LAUNCH_WAIT_S):
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
            found = landed_shelves(self.scene, self._read_log())
            if found:
                break
            self.cancel.wait(2.0)
        if not found:
            what = ("the Library never logged `libhubs: section "
                    f"{library_section_index(self.scene)} landed`"
                    if self.walk.get("landed") == "library" else "Home never logged `hubs: landed`")
            self.error = f"{what}, so there were no rows to walk"
            return
        self.rows = found
        if self.cancel.wait(float(self.walk.get("settle_s", 3))):
            return
        steps = walk_steps(self.scene, self.rows)
        proc = subprocess.Popen(self.ssh_argv(self.tv, "sh"), stdin=subprocess.PIPE, text=True,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            nxt = time.monotonic()
            for key, delay_s in steps:
                if self.cancel.is_set():
                    break
                proc.stdin.write(f"echo {key} > {self.fifo}\n")
                proc.stdin.flush()
                self.sent += 1
                nxt += delay_s
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

