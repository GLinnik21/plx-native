#!/usr/bin/env python3
"""Paging under a conditioned link, end to end on the HOST simulator (no television).

Every list in the app pages through a sliding 24-card window. That is only correct when a page
landing and a key repeat coincide correctly, and on a loopback mock a page lands before the next
key, so nothing exercised it. This drives the real simulator (`plxnative-sim`) with real keys
against `tests/mock_pms.py` behind a `tests/link_conditioner.py` profile and grades what the app
itself reports:

  * `plxnative-focus` + `plxnative-focusx`: one `focus route=home ... row= col= rk= fx=` line per
    CHANGE of focus, and (focusx) per frame the focused card's DRAWN x moves. From them: reach,
    one-card-per-key, no focus change without a key, no slot jump, the window bound (`col`).
  * `plxnative-phcount`: placeholder (skeleton) card draws per second.
  * `plxnative-framedrop`: `FRAMEDROP` lines (one per late frame) and the heartbeat's `frame_*`.
  * the mock's own wire log (`GET /_mock/wire`): when each page was asked for and finished, on
    the same monotonic clock (the mock runs in this process).

HOST NUMBERS. Frame times here are this Mac's GPU, in a debug build, and say nothing about the
television: they are reported to show WHERE late frames fall relative to page landings, never as a
smoothness result (the simulator's heartbeat carries `sim=1` for the same reason).

    python3 tests/paging_link.py                         # every surface x slow-latency, low-bandwidth
    python3 tests/paging_link.py --surface home-row --profile 3g --profile remote-wan
    python3 tests/paging_link.py --build                 # build the simulator first
    python3 tests/paging_link.py --list

Surfaces are rows of `SURFACES`; a surface the app gives no per-item read-out for is listed in
`NOT_COVERED` with the reason instead of being faked. Exit status is non-zero on any failed
assertion. Not part of `make check` (minutes, a window, real keys); `python3 tests/paging_link.py
--selftest` proves the analysis on synthetic logs in under a second and is what the unit gate runs.
"""
import argparse
import json
import os
import pathlib
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import link_conditioner  # noqa: E402

SIM = ROOT / "rust-modules" / "target-sim" / "debug" / "plxnative-sim"
WINDOW = 24            # cards a row holds at once (plx_data::pms RECENT window); read from `col`
PROFILES = ("3g", "remote-wan", "lossy")  # slow latency, a far server, a flaky one
# The two the brief names: high latency with a fat pipe, and low bandwidth with low latency.
DEFAULT_PROFILES = ("slow-latency", "low-bandwidth")
EXTRA_PROFILES = {
    "slow-latency": link_conditioner.LinkProfile(latency_ms=700, jitter_ms=250, kbit=8000),
    "low-bandwidth": link_conditioner.LinkProfile(latency_ms=40, jitter_ms=10, kbit=250),
}
JUMP_PX = 400          # a focused card that moves this far between two frames has teleported

FOCUS_RE = re.compile(r"^focus route=(\w+) (.*)$")
KV_RE = re.compile(r"(\w+)=(\S+)")
HEART_RE = re.compile(r"^loop=\d+ route=(\w+) fps=(\d+).* frame_n=(\d+) frame_gt16=(\d+) frame_gt33=(\d+)"
                      r".* frame_max=([\d.]+)ms")
DROP_RE = re.compile(r"^FRAMEDROP total=([\d.]+)")
PH_RE = re.compile(r"^phcount: frames=(\d+) ph_frames=(\d+) draws=(\d+) ph=(\d+)")

# surface -> how to boot it, which key walks it, the list it walks and how to read the focus line.
SURFACES = {
    "home-row": {
        "what": "Home > Recently Added Movies, a 24-card sliding window over the movie library",
        "route": "home", "triggers": {"plxnative-grid": ""}, "prelude": ["down"], "row": 1,
        "key": "right", "back": "left",
        "order": lambda lib: [str(it["ratingKey"]) for it in lib.recent("movie", 1000)],
    },
}
NOT_COVERED = {
    "library-section-hub-row": "no per-item rk on the shelf fingerprint (Library probe reports "
                               "region/row/col/viewport only); needs a Library shelf read-out",
    "library-grid": "Library fingerprint carries rk + viewport_y but the grid is windowed by row "
                    "bands; needs its own order map (grid sort) and the band bound read-out",
    "search-row": "Search probe reports zone/row/col only, no rk",
    "detail-related-row": "Detail content probe has no per-item rk for the related shelf",
    "collection": "Collection route probe has no per-item rk",
    "long-season": "season episodes list: no per-item read-out on the Detail season strip",
}


# ----------------------------------------------------------------- analysis (pure) -------------

def parse_focus(line):
    m = FOCUS_RE.match(line)
    if not m:
        return None
    fields = dict(KV_RE.findall(m.group(2)))
    fields["route"] = m.group(1)
    return fields


def analyse_focus(samples, keys, order, row):
    """`samples`: [(t, fields)] focus changes on `row`; `keys`: [(t, 'right'|'left')] presses.
    Returns (findings dict, failures list). Pure, so the selftest drives it with synthetic logs."""
    index = {rk: i for i, rk in enumerate(order)}
    fails, steps, last_i, last_fx, last_col = [], [], None, None, None
    max_col, max_jump, rebases = -1, 0, 0
    first_key = keys[0][0] if keys else float("inf")
    for t, f in samples:
        if int(f.get("row", -1)) != row or f.get("rk") not in index:
            continue
        col, i = int(f["col"]), index[f["rk"]]
        max_col = max(max_col, col)
        fx = int(f["fx"]) if "fx" in f else None
        if last_i is not None and i == last_i and last_col is not None and col != last_col:
            rebases += 1  # the window slid under a still focus: same card, new slot
        if last_i is not None and i != last_i:
            steps.append((t, i - last_i))
            if t < first_key:
                fails.append(f"focus moved at t={t:.3f} before any key")
        if fx is not None and last_fx is not None and last_i == i:
            jump = abs(fx - last_fx)
            max_jump = max(max_jump, jump)
            if jump > JUMP_PX:
                fails.append(f"slot jump: card {f['rk']} moved {fx - last_fx:+d}px in one frame "
                             f"(col {last_col}->{col}) at t={t:.3f}")
        last_i, last_fx, last_col = i, fx, col
    for t, d in steps:
        if abs(d) != 1:
            fails.append(f"focus skipped {d:+d} cards at t={t:.3f}")
    if len(steps) > len(keys):
        fails.append(f"focus changed {len(steps)} times for {len(keys)} key presses")
    if max_col >= WINDOW:
        fails.append(f"window bound broken: col {max_col} >= {WINDOW}")
    reached = [index[f["rk"]] for _, f in samples if int(f.get("row", -1)) == row and f.get("rk") in index]
    return {"focus_changes": len(steps), "max_col": max_col, "max_jump_px": max_jump,
            "rebases": rebases, "first": min(reached) if reached else None,
            "last": max(reached) if reached else None}, fails


def place_late_frames(landings, late, window=(-0.05, 0.25)):
    """Late frames inside `window` seconds of any landing vs elsewhere: (near, away)."""
    near = sum(any(l + window[0] <= t <= l + window[1] for l in landings) for t in late)
    return near, len(late) - near


# ----------------------------------------------------------------- the run -------------------

class SimRun:
    """One simulator process in its own runtime dir; every event-log line is stamped with the
    monotonic time THIS process first saw it (the log itself carries no clock)."""

    def __init__(self, binary, port, triggers, tmp):
        self.dir = pathlib.Path(tmp)
        self.lines = []  # (t, text)
        self.lock = threading.Lock()
        self.stop = threading.Event()
        (self.dir / "plxnative-token").write_text("x")
        for name, value in triggers.items():
            (self.dir / name).write_text(str(value))
        env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=str(self.dir), PLXNATIVE_APP_DIR=str(ROOT / "pkg"))
        self.proc = subprocess.Popen([str(binary), "127.0.0.1", str(port)], env=env,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.thread = threading.Thread(target=self._tail, daemon=True)
        self.thread.start()
        self.fifo = None

    def _tail(self):
        log = self.dir / "plxnative-events.log"
        while not log.exists() and not self.stop.is_set():
            time.sleep(0.01)
        buf = b""
        with open(log, "rb") as f:
            while not self.stop.is_set():
                chunk = f.read(65536)
                if not chunk:
                    time.sleep(0.002)
                    continue
                now = time.monotonic()
                buf += chunk
                *whole, buf = buf.split(b"\n")
                with self.lock:
                    self.lines.extend((now, w.decode("utf-8", "replace")) for w in whole)

    def snapshot(self):
        with self.lock:
            return list(self.lines)

    def wait_for(self, pattern, timeout, after=0):
        deadline = time.monotonic() + timeout
        rx = re.compile(pattern)
        while time.monotonic() < deadline:
            for t, line in self.snapshot():
                if t >= after and rx.search(line):
                    return t
            time.sleep(0.05)
        return None

    def key(self, name):
        if self.fifo is None:
            self.fifo = os.open(self.dir / "plxnative-remote", os.O_RDWR)
        os.write(self.fifo, (name + " ").encode())
        return time.monotonic()

    def close(self):
        self.stop.set()
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        if self.fifo is not None:
            os.close(self.fifo)


def wire(base):
    with urllib.request.urlopen(base + "/_mock/wire", timeout=5) as r:
        return json.loads(r.read())["rows"]


def run_surface(name, profile_name, args):
    import mock_pms
    spec = SURFACES[name]
    profile = EXTRA_PROFILES.get(profile_name) or link_conditioner.PROFILES[profile_name]
    srv, pms = mock_pms.serve(0, seed=args.seed, movies=args.movies,
                              link=[])  # fast during boot; the profile is applied once Home is up
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    order = spec["order"](pms.lib)
    tmp = tempfile.mkdtemp(prefix="plx-paging-")
    triggers = dict(spec["triggers"], **{"plxnative-focus": "", "plxnative-focusx": "",
                                         "plxnative-phcount": "", "plxnative-framedrop": "17"})
    sim = SimRun(args.sim, srv.server_address[1], triggers, tmp)
    fails, keys, result = [], [], {}
    try:
        if not sim.wait_for(r"^hubs: landed", 40):
            return {"surface": name, "profile": profile_name}, ["Home never landed"]
        time.sleep(3)
        for k in spec["prelude"]:
            sim.key(k)
            time.sleep(0.8)
        pms.link.set("all", link_conditioner.make_profile(profile))
        link_conditioner  # the link is live from here on
        base_i = len(sim.snapshot())
        t0 = time.monotonic()
        gap = args.gap_ms / 1000

        def focus_now():
            for t, line in reversed(sim.snapshot()):
                f = parse_focus(line)
                if f and int(f.get("row", -1)) == spec["row"] and f.get("rk") in order:
                    return order.index(f["rk"])
            return None

        def walk(key, target, label):
            """Press `key` every `gap` until the focus is on `target`; give up after a time bound
            derived from the profile (every card one key, plus every page its worst round trip)."""
            pages = len(order) / 12
            bound = len(order) * gap * 1.5 + pages * (profile.latency_ms + profile.jitter_ms) / 1000 * 3 + 60
            end = time.monotonic() + bound
            while time.monotonic() < end:
                cur = focus_now()
                if cur == target:
                    return True
                keys.append((sim.key(key), key))
                time.sleep(gap)
            fails.append(f"{label}: did not reach item {target} (stuck at {focus_now()}) in {bound:.0f}s")
            return False

        walk(spec["key"], len(order) - 1, "forward")
        time.sleep(2)
        walk(spec["back"], 0, "back")
        # at rest: every placeholder must resolve within a bound derived from the profile
        rest_bound = 3 + 2 * (profile.latency_ms + profile.jitter_ms) / 1000 + \
            24 * 1.0 * 14000 * 8 / max(profile.kbit * 1000, 1) if profile.kbit else 5
        time.sleep(rest_bound)
        t_end = time.monotonic()
        lines = sim.snapshot()
        rows = wire(base)
    finally:
        sim.close()
        srv.shutdown()
        srv.server_close()
        shutil.rmtree(tmp, ignore_errors=True)

    samples = [(t, f) for t, line in lines if t >= t0 for f in [parse_focus(line)] if f]
    if args.dump:
        with open(args.dump, "a") as out:
            out.write(f"## {name} / {profile_name}\n")
            out.writelines(f"{t - t0:9.3f} {line}\n" for t, line in lines if t >= t0 and
                           (line.startswith(("focus", "FRAMEDROP", "phcount", "hubs:")) or "key type=0x300" in line))
            out.writelines(f"{r['t_recv'] - t0:9.3f} WIRE {r['class']} {r['path']} start={r['start']} "
                           f"done=+{(r['t_done'] - r['t_recv']) * 1000:.0f}ms failed={r['failed']}\n" for r in rows)
    finding, f2 = analyse_focus(samples, keys, order, spec["row"])
    fails += f2
    if finding["last"] != len(order) - 1:
        fails.append(f"reach: the last item ({len(order) - 1}) was never focused (max {finding['last']})")
    if finding["first"] != 0:
        fails.append(f"reach: walking back never reached the first item (min {finding['first']})")
    # keys that moved nothing: a press with no focus change before the next press
    change_ts = []
    prev = None
    for t, f in samples:
        if f.get("rk") in order and int(f.get("row", -1)) == spec["row"]:
            if prev is not None and f["rk"] != prev:
                change_ts.append(t)
            prev = f["rk"]
    wasted = sum(1 for (t, _), nxt in zip(keys, [k[0] for k in keys[1:]] + [t_end])
                 if not any(t <= c < nxt + 0.05 for c in change_ts))
    # frames
    beats = [m for t, line in lines if t >= t0 for m in [HEART_RE.match(line)] if m]
    frames = sum(int(m.group(3)) for m in beats)
    gt16 = sum(int(m.group(4)) for m in beats)
    gt33 = sum(int(m.group(5)) for m in beats)
    worst = max([float(m.group(6)) for m in beats] or [0])
    late = [t for t, line in lines if t >= t0 and DROP_RE.match(line)]
    pages = [r for r in rows if r["class"] == "listing" and r["start"] not in (None, "0") and not r["failed"]]
    landings = [r["t_done"] for r in pages]
    near, away = place_late_frames(landings, late)
    failed_reads = [r for r in rows if r["failed"]]
    # a failed read must be asked for again, and succeed
    for r in failed_reads:
        again = [x for x in rows if x["path"] == r["path"] and x["start"] == r["start"]
                 and not x["failed"] and x["t_recv"] > r["t_recv"]]
        if not again and r["class"] == "listing":
            fails.append(f"a failed read of {r['path']} start={r['start']} was never retried")
    ph = [int(m.group(2)) for t, line in lines if t >= t0 for m in [PH_RE.match(line)] if m]
    ph_at_rest = [int(m.group(2)) for t, line in lines if t >= t_end - rest_bound + 1 for m in [PH_RE.match(line)] if m]
    if ph_at_rest and ph_at_rest[-1] > 0:
        fails.append(f"placeholders still on screen {rest_bound:.0f}s after the last key (ph_frames={ph_at_rest[-1]})")
    # per-page timeline: ask -> landed -> window slid (a rebase focus line) -> posters resolved
    slid = [t for t, f in samples if False]
    timeline = []
    images = [r for r in rows if r["class"] == "image"]
    for r in sorted(pages, key=lambda r: r["t_recv"])[:12]:
        after = [x["t_done"] for x in images if x["t_recv"] >= r["t_done"] - 0.01 and x["t_done"] < r["t_done"] + 6]
        timeline.append({"start": r["start"], "ask_to_landed_ms": round((r["t_done"] - r["t_recv"]) * 1000),
                         "landed_to_last_poster_ms": round((max(after) - r["t_done"]) * 1000) if after else None})
    result = {"surface": name, "profile": profile_name, "link": profile.to_json(),
              "keys": len(keys), "keys_that_moved_nothing": wasted, "focus": finding,
              "host_frames": {"frames": frames, "late_gt16": gt16, "late_gt33": gt33, "worst_ms": worst,
                              "late_frames_logged": len(late), "late_within_-50..+250ms_of_a_landing": near,
                              "late_elsewhere": away},
              "pages_landed": len(pages), "failed_reads": len(failed_reads),
              "placeholder_frames_per_second_max": max(ph or [0]),
              "placeholder_seconds": sum(1 for p in ph if p), "page_timeline": timeline}
    return result, fails


def selftest():
    order = [str(100 + i) for i in range(60)]

    def line(rk, col, fx, row=1):
        return (0, {"route": "home", "row": str(row), "col": str(col), "rk": rk, "fx": str(fx)})
    good = [line(str(100 + i), min(i, 12 + (i % 12)), 900 - (i % 3) * 20) for i in range(30)]
    good = [(1.0 + i * 0.1, f) for i, (_, f) in enumerate(good)]
    keys = [(0.95 + i * 0.1, "right") for i in range(40)]
    _, fails = analyse_focus(good, keys, order, 1)
    assert not fails, fails
    # a teleport: same card, drawn 1200 px away in one frame
    bad = list(good) + [(5.0, dict(good[-1][1], fx="-300"))]
    _, fails = analyse_focus(bad, keys, order, 1)
    assert any("slot jump" in f for f in fails), fails
    # skipped card, focus without a key, window overrun
    skip = [good[0], (1.1, dict(good[1][1], rk="103"))]
    assert any("skipped" in f for f in analyse_focus(skip, keys, order, 1)[1])
    assert any("before any key" in f for f in analyse_focus(good, [(9.0, "right")], order, 1)[1])
    over = [(1.0, dict(good[0][1], col="24"))]
    assert any("window bound" in f for f in analyse_focus(over, keys, order, 1)[1])
    assert place_late_frames([10.0], [10.1, 12.0]) == (1, 1)
    assert parse_focus("focus route=home snapt=1 row=1 col=3 rk=44 fx=-12")["fx"] == "-12"
    print("paging_link selftest: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--surface", action="append", help=f"one of {', '.join(SURFACES)} (default all)")
    ap.add_argument("--profile", action="append",
                    help=f"link profile (default {', '.join(DEFAULT_PROFILES)}); any of "
                         f"{', '.join(sorted(link_conditioner.PROFILES))}, {', '.join(EXTRA_PROFILES)}")
    ap.add_argument("--movies", type=int, default=200)
    ap.add_argument("--gap-ms", type=int, default=125, help="key repeat period (a held key is ~125)")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--sim", type=pathlib.Path, default=SIM)
    ap.add_argument("--build", action="store_true", help="cargo build the simulator first")
    ap.add_argument("--json", type=pathlib.Path, help="write the full results here")
    ap.add_argument("--dump", type=pathlib.Path, help="append the stamped focus/frame/wire lines here")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    if args.list:
        for n, s in SURFACES.items():
            print(f"{n}: {s['what']}")
        for n, why in NOT_COVERED.items():
            print(f"{n}: NOT COVERED - {why}")
        return 0
    if args.build:
        subprocess.check_call(["cargo", "build", "--manifest-path", str(ROOT / "rust-modules/Cargo.toml"),
                               "--target-dir", str(ROOT / "rust-modules/target-sim"), "--features",
                               "hostsim", "--bin", "plxnative-sim"], env=dict(os.environ, CARGO_INCREMENTAL="0"))
    if not args.sim.exists():
        sys.exit(f"no simulator at {args.sim}; run with --build")
    results, bad = [], 0
    for surface in args.surface or SURFACES:
        for profile in args.profile or DEFAULT_PROFILES:
            res, fails = run_surface(surface, profile, args)
            res["failures"] = fails
            results.append(res)
            bad += bool(fails)
            print(json.dumps(res, indent=1))
            for f in fails:
                print(f"FAIL [{surface} / {profile}] {f}")
    if args.json:
        args.json.write_text(json.dumps(results, indent=1))
    print(f"paging_link: {len(results)} runs, {bad} failing")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
