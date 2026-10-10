#!/usr/bin/env python3
"""Paging under a conditioned link, end to end on the HOST simulator (no television).

Every list in the app pages through a sliding window of cards. That is only correct when a page
landing and a key repeat coincide correctly, and on a loopback mock a page lands before the next
key, so nothing exercised it. This drives the real simulator (`plxnative-sim`) with real keys
against `tests/mock_pms.py` behind a `tests/link_conditioner.py` profile and grades what the app
itself reports:

  * `plxnative-focus` + `plxnative-focusx`: one `focus route=... ` line per CHANGE of focus and,
    with focusx, per frame the focused card moves. Every page that draws a card section
    (`plx_ui::card_probe`) adds `cdx cdy cdw` (the card's DRAWN screen rect), `cdi cdn` (its slot
    in the run of cards the section holds and that run's length), `cdg` (index in the source),
    `cdk` (the ratingKey in its artwork path: the stable key), `cda` (texture resident), `cdr`
    (draws of a poster that was showing and went back to its placeholder, running total) and
    `cdc cdcw` (the caption's left edge and width). From them: reach, one-card-per-key, no focus
    change without a key, no slot jump, the window bound, off-canvas frames, art regressions.
  * `plxnative-phcount`: placeholder (skeleton) card draws per second.
  * `plxnative-framedrop`: `FRAMEDROP` lines (one per late frame) and the heartbeat's `frame_*`.
  * the mock's own wire log (`GET /_mock/wire`): when each page was asked for and finished, on
    the same monotonic clock (the mock runs in this process).

HOST NUMBERS. Frame times here are this Mac's GPU, in a debug build, and say nothing about the
television: they are reported to show WHERE late frames fall relative to page landings, never as a
smoothness result (the simulator's heartbeat carries `sim=1` for the same reason).

    python3 tests/paging_link.py                         # every surface x the default profiles
    python3 tests/paging_link.py --surface library-grid --profile remote-wan --profile lossy-503
    python3 tests/paging_link.py --build                 # build the simulator first
    python3 tests/paging_link.py --list

Each surface boots straight onto its screen (`--screen`-style triggers, no UI walking), presses
`seat` until the walked list is focused, holds `key` to the end, then `back` to the start.
Exit status is non-zero on any failed assertion. Not part of `make check` (minutes, a window, real
keys); `python3 tests/paging_link.py --selftest` proves the analysis on synthetic logs in under a
second and is what the unit gate runs.
"""
import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import link_conditioner  # noqa: E402

SIM = ROOT / "rust-modules" / "target-sim" / "debug" / "plxnative-sim"
CANVAS_W, CANVAS_H = 1920, 1080  # plx_base::surface::LOGICAL_W / LOGICAL_H: what every x/y here is in
EXTRA_PROFILES = {
    # High latency with a fat pipe, low bandwidth with low latency (the first two pass on the Home
    # tip), a far server (the shape the owner saw the Home defect on) and the two error shapes.
    "slow-latency": link_conditioner.LinkProfile(latency_ms=700, jitter_ms=250, kbit=8000),
    "low-bandwidth": link_conditioner.LinkProfile(latency_ms=40, jitter_ms=10, kbit=250),
    "lossy-503": link_conditioner.LinkProfile(latency_ms=100, jitter_ms=50, kbit=2000, error_rate=0.1,
                                              error_kind="503"),
}
DEFAULT_PROFILES = ("none", "remote-wan", "slow-latency", "low-bandwidth", "lossy")
JUMP_PX = 400          # a focused card that moves this far between two frames has teleported
RETRY_WITHIN_S = 45    # a failed page must be asked for again, and answered, within this

FOCUS_RE = re.compile(r"^focus route=(\w+) (.*)$")
KV_RE = re.compile(r"(\w+)=(\S+)")
HEART_RE = re.compile(r"^loop=\d+ route=(\w+) fps=(\d+).* frame_n=(\d+) frame_gt16=(\d+) frame_gt33=(\d+)"
                      r".* frame_max=([\d.]+)ms")
DROP_RE = re.compile(r"^FRAMEDROP total=([\d.]+)")
PH_RE = re.compile(r"^phcount: frames=(\d+) ph_frames=(\d+) draws=(\d+) ph=(\d+)")
RK_IN_URL = re.compile(r"/library/(?:metadata|collections)/(\d+)/")


def _recent(lib, kind="movie"):
    return [str(it["ratingKey"]) for it in lib.recent(kind, 1000)]


def _grid_order(lib, pms):
    """The Library grid's order: the mock's own answer to the listing query the app made."""
    for r in pms.request_log:
        q = r["query"]
        if r["path"] == "/library/sections/1/all" and q.get("type") != "18" and "X-Plex-Container-Start" in q:
            return [str(it["ratingKey"]) for it in lib.section_items("1", q)]
    raise RuntimeError("the app never asked for a Library grid listing")


# surface -> how to boot it, what the walked list is and how to read it. `pick` selects the focus
# lines that belong to the walked list; `order` the list in the order the server serves it;
# `stride` how many items one `key` moves (a grid row); `window_max` the bound on `cdn`.
SURFACES = {
    "home-row": {
        "what": "Home > Recently Added Movies, a 24-card sliding window over the movie library",
        "mock": {"movies": 200}, "triggers": {"plxnative-grid": ""},
        "pick": lambda f: f["route"] == "home" and f.get("row") == "1",
        "seat": "down", "key": "right", "back": "left", "stride": 1, "window_max": 24,
        "pages": r"^/library/sections/1/recentlyAdded$",
        "order": lambda lib, pms: _recent(lib),
    },
    "library-section-hub-row": {
        "what": "Library > Movies > a pageable section shelf (a Shelf; the hub's own key serves the listing)",
        "mock": {"movies": 300, "section_hubs": 3}, "triggers": {"plxnative-library": "0"},
        "pick": lambda f: f["route"] == "library" and f.get("region") == "shelf" and f.get("row") == "2",
        "seat": "down", "key": "right", "back": "left", "stride": 1, "window_max": 24,
        "pages": r"^/hubs/mock/section/1/shelf/",
        "order": lambda lib, pms: _recent(lib),
    },
    "library-grid": {
        "what": "Library > Movies, the poster grid (6 columns, row-band window over the section listing)",
        "mock": {"movies": 1000}, "triggers": {"plxnative-library": "0"},
        "pick": lambda f: f["route"] == "library" and f.get("region") == "grid",
        "seat": "down", "key": "down", "back": "up", "stride": 6, "window_max": None,
        "pages": r"^/library/sections/1/all$",
        "order": _grid_order,
    },
    "search-row": {
        "what": "Search > the Movies result row for a 2-letter query (preview of 12, then the typed listing)",
        "mock": {"movies": 1000}, "triggers": {"plxnative-search": "sf"},
        "pick": lambda f: f["route"] == "search" and f.get("zone") == "Results" and f.get("row") == "0",
        "seat": "down", "key": "right", "back": "left", "stride": 1, "window_max": 24,
        "pages": r"^/library/search$",
        "order": lambda lib, pms: [str(r["Metadata"]["ratingKey"]) for r in lib.search_results("sf", ["movies"])],
    },
    "detail-related-row": {
        "what": "Detail (a movie) > the related row (head of 8, then the similar listing)",
        "mock": {"movies": 300}, "triggers": {"plxnative-detail": "1001", "plxnative-detailsec": "2"},
        "pick": lambda f: f["route"] == "detail" and f.get("sec") == "3",
        "seat": None, "key": "right", "back": "left", "stride": 1, "window_max": 32,
        "pages": r"^/library/metadata/1001/similar$",
        "order": lambda lib, pms: [str(it["ratingKey"]) for it in lib.similar(1001)],
    },
    "collection": {
        "what": "Collection page (a collection of ~130 members; a 6-column Grid over a paged listing)",
        "mock": {"movies": 1000}, "triggers": {"plxnative-collection": "50002"},
        "pick": lambda f: f["route"] == "collection" and f.get("card") == "1",
        "seat": "down", "key": "down", "back": "up", "stride": 6, "window_max": None,
        "pages": r"^/library/collections/50002/children$",
        "order": lambda lib, pms: [str(it["ratingKey"]) for it in lib.collection_rows(2)],
    },
    "long-season": {
        "what": "Detail (a show) > the episode strip of a 150-episode season (pages of 60)",
        "mock": {"movies": 10, "shows": 2, "seasons": 2, "episodes": 150},
        "triggers": {"plxnative-detail": "2001"},
        "pick": lambda f: f["route"] == "detail" and f.get("sec") == "2" and f.get("ep", "-") != "-",
        "seat": "down", "key": "right", "back": "left", "stride": 1, "window_max": None,
        "pages": r"^/library/metadata/20011/children$",
        "order": lambda lib, pms: [str(it["ratingKey"]) for it in lib.children(20011)],
    },
}
NOT_COVERED = {}


# ----------------------------------------------------------------- analysis (pure) -------------

def parse_focus(line):
    m = FOCUS_RE.match(line)
    if not m:
        return None
    fields = dict(KV_RE.findall(m.group(2)))
    fields["route"] = m.group(1)
    return fields


def item_of(f):
    """The stable key of the focused item: the card's artwork key, else the fingerprint's `rk`."""
    k = f.get("cdk")
    return k if k not in (None, "-") else f.get("rk")


def _px(f, a, b=None):
    """The drawn x (`cdx`, else Home's `fx`), as int, or None."""
    for key in (a, b):
        if key and key in f:
            try:
                return int(f[key])
            except ValueError:
                return None
    return None


def analyse_focus(samples, keys, order, stride=1, window_max=None):
    """`samples`: [(t, fields)] focus lines of the walked list; `keys`: [(t, key)] presses.
    Returns (findings dict, failures list). Pure, so the selftest drives it with synthetic logs.
    A sample without a card read-out (an idle frame logged the fingerprint without drawing) only
    contributes its focus change, never a position."""
    index = {rk: i for i, rk in enumerate(order)}
    last = len(order) - 1
    fails, steps, last_i, last_x, last_y, last_t, last_slot = [], [], None, None, None, None, None
    max_cdn, max_jump, rebases, regress = 0, 0, 0, 0
    off = {"card_partial": 0, "card_off": 0, "caption_partial": 0, "caption_off": 0,
           "min_x": None, "max_right": None, "min_y": None, "max_y": None, "card_off_y": 0, "frames": 0}
    first_key = keys[0][0] if keys else float("inf")
    for t, f in samples:
        rk = item_of(f)
        if rk not in index:
            continue
        i = index[rk]
        x, y, w = _px(f, "cdx", "fx"), _px(f, "cdy"), _px(f, "cdw")
        slot = int(f["cdi"]) if "cdi" in f else (int(f["col"]) if "col" in f else None)
        if "cdn" in f:
            max_cdn = max(max_cdn, int(f["cdn"]))
        if "cdr" in f:
            regress = max(regress, int(f["cdr"]))
        if last_i is not None and i == last_i and last_slot is not None and slot is not None and slot != last_slot:
            rebases += 1  # the window slid under a still focus: same card, new slot
        if last_i is not None and i != last_i:
            steps.append((t, i - last_i, i))
            if t < first_key:
                fails.append(f"focus moved at t={t:.3f} before any key")
        if x is not None and w is not None:
            off["frames"] += 1
            off["min_x"] = x if off["min_x"] is None else min(off["min_x"], x)
            off["max_right"] = x + w if off["max_right"] is None else max(off["max_right"], x + w)
            if x < 0 or x + w > CANVAS_W:
                off["card_off" if x + w <= 0 or x >= CANVAS_W else "card_partial"] += 1
            if "cdc" in f and "cdcw" in f:
                cx, cw = int(f["cdc"]), int(f["cdcw"])
                if cx < 0 or cx + cw > CANVAS_W:
                    off["caption_off" if cx + cw <= 0 or cx >= CANVAS_W else "caption_partial"] += 1
        if y is not None:
            off["min_y"] = y if off["min_y"] is None else min(off["min_y"], y)
            off["max_y"] = y if off["max_y"] is None else max(off["max_y"], y)
            if y < 0 or y >= CANVAS_H:
                off["card_off_y"] += 1  # the card's top edge is above the canvas or below its bottom
        if last_i == i:
            for axis, now, before in (("x", x, last_x), ("y", y, last_y)):
                if now is not None and before is not None:
                    max_jump = max(max_jump, abs(now - before))
                    if abs(now - before) > JUMP_PX:
                        fails.append(f"slot jump: card {rk} moved {now - before:+d}px in {axis} in one frame "
                                     f"(dt {t - last_t:.3f}s, slot {last_slot}->{slot}) at t={t:.3f}")
        last_i, last_t = i, t
        if x is not None:
            last_x, last_y = x, y if y is not None else last_y
            last_slot = slot if slot is not None else last_slot
        elif slot is not None:
            last_slot = slot
    skips, coalesced, prev_t = 0, 0, first_key
    for t, d, to in steps:
        # Keys held while the list cannot move are applied together on the frame it can: one step
        # may cover every key pressed since the last one, never more (that would invent moves).
        pressed = max(1, sum(1 for kt, _ in keys if prev_t < kt <= t))
        prev_t = t
        if pressed > 1 and abs(d) > stride:
            coalesced += 1
        ok = abs(d) <= stride * pressed and (abs(d) % stride == 0 or to in (0, last) or stride == 1)
        if not ok:
            skips += 1
            fails.append(f"focus skipped {d:+d} cards ({pressed} keys since the last move, one key moves "
                         f"{stride}) at t={t:.3f}")
    if len(steps) > len(keys):
        fails.append(f"focus changed {len(steps)} times for {len(keys)} key presses")
    if window_max is not None and max_cdn > window_max:
        fails.append(f"window bound broken: the section held {max_cdn} cards > {window_max}")
    reached = [index[item_of(f)] for _, f in samples if item_of(f) in index]
    tail = (last // stride) * stride if order else 0
    return {"focus_changes": len(steps), "skips": skips, "coalesced_steps": coalesced, "max_cdn": max_cdn, "max_jump_px": max_jump,
            "rebases": rebases, "first": min(reached) if reached else None,
            "last": max(reached) if reached else None, "tail_row_start": tail,
            "art_regressions": regress, "off_canvas": off}, fails


def place_late_frames(landings, late, window=(-0.05, 0.25)):
    """Late frames inside `window` seconds of any landing vs elsewhere: (near, away)."""
    near = sum(any(l + window[0] <= t <= l + window[1] for l in landings) for t in late)
    return near, len(late) - near


def page_figures(rows, pages_rx, order, stride=1):
    """Per listing page: ask -> landed, and landed -> the last poster of THAT page's items that was
    asked for within the next 8 s, finished (`None` when none was asked for: the focus had passed).
    A page's items are `order[start:start+size]`; a poster belongs to it by the ratingKey in the
    transcode's `url`. Pure over the mock's wire rows."""
    rx = re.compile(pages_rx)
    done, out = [], []
    posters = {}
    for r in rows:
        if r["class"] == "image" and not r["failed"] and r.get("url"):
            m = RK_IN_URL.search(urllib.parse.unquote(r["url"]))
            if m:
                posters.setdefault(m.group(1), []).append(r)
    pages = sorted((r for r in rows if r["class"] == "listing" and rx.search(r["path"]) and not r["failed"]
                    and r["start"] not in (None, "0")), key=lambda r: r["t_recv"])
    for r in pages:
        start, size = int(r["start"]), int(r["size"] or 0)
        mine = order[start:start + size]
        fin = [x["t_done"] for rk in mine for x in posters.get(rk, [])
               if r["t_done"] - 0.01 <= x["t_recv"] <= r["t_done"] + 8]
        asked = sum(1 for rk in mine if any(r["t_done"] - 0.01 <= x["t_recv"] <= r["t_done"] + 8
                                            for x in posters.get(rk, [])))
        out.append({"start": start, "ask_to_landed_ms": round((r["t_done"] - r["t_recv"]) * 1000),
                    "posters_asked_within_8s": asked, "of": len(mine),
                    "landed_to_last_of_its_posters_ms": round((max(fin) - r["t_done"]) * 1000) if fin else None})
        done.append(r["t_done"])
    return out, done


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

    def latest(self, pred):
        """The newest line for which `pred(t, line)` holds, without copying the log."""
        with self.lock:
            for t, line in reversed(self.lines):
                if pred(t, line):
                    return t, line
        return None

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


def resolve_profile(name):
    return EXTRA_PROFILES.get(name) or link_conditioner.PROFILES[name]


def run_surface(name, profile_name, args):
    import mock_pms
    spec = SURFACES[name]
    profile = resolve_profile(profile_name)
    mock = dict(spec["mock"])
    if args.movies is not None:
        mock["movies"] = args.movies
    srv, pms = mock_pms.serve(0, seed=args.seed, link=[], **mock)  # fast during boot; the profile is applied once seated
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    tmp = tempfile.mkdtemp(prefix="plx-paging-")
    triggers = dict(spec["triggers"], **{"plxnative-focus": "", "plxnative-focusx": "",
                                         "plxnative-phcount": "", "plxnative-framedrop": "17"})
    sim = SimRun(args.sim, srv.server_address[1], triggers, tmp)
    fails, keys, result = [], [], {}
    order = []
    try:
        if not sim.wait_for(r"^(hubs: landed|focus route=)", 60):
            return {"surface": name, "profile": profile_name}, ["the app never reached its first screen"]
        time.sleep(3)

        def focus_now():
            """Index in `order` of the focused item (the newest focus line that names one of the
            walked list's items: a frame that drew no card names only the page's own key)."""
            index = {rk: i for i, rk in enumerate(order)}
            hit = sim.latest(lambda t, line: line.startswith("focus ") and (lambda f: f is not None
                             and spec["pick"](f) and item_of(f) in index)(parse_focus(line)))
            return None if hit is None else index[item_of(parse_focus(hit[1]))]

        # Seat: press `seat` until the walked list holds the focus; the list is known only once
        # the app has asked for it (the grid's order comes from the listing query it sent).
        deadline = time.monotonic() + 40
        seated = False
        while time.monotonic() < deadline:
            try:
                order = spec["order"](pms.lib, pms)
            except RuntimeError:
                order = []
            if order and focus_now() is not None:
                seated = True
                break
            if spec["seat"] and order:
                sim.key(spec["seat"])
            time.sleep(0.9)
        if not seated:
            return {"surface": name, "profile": profile_name}, ["never seated on the walked list (no focus line "
                                                                 "matched, or its item is not in the server's order)"]
        first_seen = focus_now()
        if first_seen not in range(0, spec["stride"]):
            fails.append(f"seated at item {first_seen}, not on the first row: the order model may be wrong")
        time.sleep(1.0)
        pms.link.set("all", link_conditioner.make_profile(profile))
        t0 = time.monotonic()
        gap = args.gap_ms / 1000
        stride = spec["stride"]
        tail = ((len(order) - 1) // stride) * stride

        def walk(key, done, label):
            """Press `key` every `gap` until `done(cur)`; give up after a bound derived from the
            profile (every key, plus every page its worst round trip, times three)."""
            pages = len(order) / 12
            bound = len(order) / stride * gap * 1.5 + pages * (profile.latency_ms + profile.jitter_ms) / 1000 * 3 + 60
            end = time.monotonic() + bound
            while time.monotonic() < end:
                cur = focus_now()
                if cur is not None and done(cur):
                    return True
                keys.append((sim.key(key), key))
                time.sleep(gap)
            fails.append(f"{label}: did not reach the {'last' if key == spec['key'] else 'first'} row "
                         f"(stuck at {focus_now()} of {len(order)}) in {bound:.0f}s")
            return False

        walk(spec["key"], lambda cur: cur >= tail, "forward")
        time.sleep(2)
        walk(spec["back"], lambda cur: cur < stride, "back")
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

    samples = [(t, f) for t, line in lines if t >= t0 for f in [parse_focus(line)] if f and spec["pick"](f)]
    if args.dump:
        with open(args.dump, "a") as out:
            out.write(f"## {name} / {profile_name}\n")
            out.writelines(f"{t - t0:9.3f} {line}\n" for t, line in lines if t >= t0 and
                           (line.startswith(("focus", "FRAMEDROP", "phcount", "hubs:")) or "key type=0x300" in line))
            out.writelines(f"{r['t_recv'] - t0:9.3f} WIRE {r['class']} {r['path']} start={r['start']} "
                           f"done=+{(r['t_done'] - r['t_recv']) * 1000:.0f}ms failed={r['failed']}\n" for r in rows)
    finding, f2 = analyse_focus(samples, keys, order, stride, spec["window_max"])
    fails += f2
    if finding["last"] is None or finding["last"] < tail:
        fails.append(f"reach: the last row ({tail}..{len(order) - 1}) was never focused (max {finding['last']})")
    if finding["first"] is None or finding["first"] >= stride:
        fails.append(f"reach: walking back never reached the first row (min {finding['first']})")
    if finding["art_regressions"]:
        fails.append(f"art: {finding['art_regressions']} draws of a poster that was showing and went back to "
                     f"its placeholder (cdr)")
    # keys that moved nothing: a press with no focus change before the next press
    change_ts, prev = [], None
    for t, f in samples:
        rk = item_of(f)
        if rk in order:
            if prev is not None and rk != prev:
                change_ts.append(t)
            prev = rk
    wasted = sum(1 for (t, _), nxt in zip(keys, [k[0] for k in keys[1:]] + [t_end])
                 if not any(t <= c < nxt + 0.05 for c in change_ts))
    # frames
    beats = [m for t, line in lines if t >= t0 for m in [HEART_RE.match(line)] if m]
    frames = sum(int(m.group(3)) for m in beats)
    gt16 = sum(int(m.group(4)) for m in beats)
    gt33 = sum(int(m.group(5)) for m in beats)
    worst = max([float(m.group(6)) for m in beats] or [0])
    late = [t for t, line in lines if t >= t0 and DROP_RE.match(line)]
    timeline, landings = page_figures(rows, spec["pages"], order, stride)
    near, away = place_late_frames(landings, late)
    # Errors: the retry assertion is only real if a failure was injected into the pages. A failed
    # page read must be asked for again, and answered, soon; a profile that fails no page proves nothing.
    pages_rx = re.compile(spec["pages"])
    failed_pages = [r for r in rows if r["failed"] and r["class"] == "listing"]
    failed_reads = [r for r in rows if r["failed"]]
    retried = redundant = 0
    for r in failed_pages:
        again = [x for x in rows if x["path"] == r["path"] and x["start"] == r["start"]
                 and not x["failed"] and x["t_recv"] > r["t_recv"]]
        ever = [x for x in rows if x["path"] == r["path"] and x["start"] == r["start"] and not x["failed"]]
        if not again and ever:
            redundant += 1  # a refetch of a page that had already landed; the at-rest placeholder check covers it
        elif not again:
            fails.append(f"a failed read of {r['path']} start={r['start']} was never retried")
        elif min(x["t_done"] for x in again) - r["t_done"] > RETRY_WITHIN_S:
            fails.append(f"a failed read of {r['path']} start={r['start']} was retried only "
                         f"{min(x['t_done'] for x in again) - r['t_done']:.0f}s later")
        else:
            retried += 1
    walked_failed = [r for r in failed_pages if pages_rx.search(r["path"]) and r["start"] not in (None, "0")]
    if profile.error_rate and not walked_failed:
        fails.append("the error profile failed none of this list's pages, so its retry/reach assertion is "
                     f"vacuous ({len(failed_reads)} reads failed in all, {len(failed_pages)} listings)")
    ph = [int(m.group(2)) for t, line in lines if t >= t0 for m in [PH_RE.match(line)] if m]
    ph_at_rest = [int(m.group(2)) for t, line in lines if t >= t_end - rest_bound + 1 for m in [PH_RE.match(line)] if m]
    if ph_at_rest and ph_at_rest[-1] > 0:
        fails.append(f"placeholders still on screen {rest_bound:.0f}s after the last key (ph_frames={ph_at_rest[-1]})")
    result = {"surface": name, "profile": profile_name, "link": profile.to_json(), "items": len(order),
              "keys": len(keys), "keys_that_moved_nothing": wasted, "focus": finding,
              "host_frames": {"frames": frames, "late_gt16": gt16, "late_gt33": gt33, "worst_ms": worst,
                              "late_frames_logged": len(late), "late_within_-50..+250ms_of_a_landing": near,
                              "late_elsewhere": away},
              "pages_landed": len(timeline), "failed_reads": len(failed_reads),
              "failed_pages": len(failed_pages), "failed_pages_retried_ok": retried, "failed_refetches_not_retried": redundant,
              "max_ask_to_landed_ms": max([p["ask_to_landed_ms"] for p in timeline] or [0]),
              "placeholder_frames_per_second_max": max(ph or [0]),
              "placeholder_seconds": sum(1 for p in ph if p), "page_timeline": timeline[:12]}
    return result, fails


def selftest():
    order = [str(100 + i) for i in range(60)]

    def line(rk, col, x, y=300, w=270, **extra):
        f = {"route": "home", "row": "1", "col": str(col), "rk": rk, "cdk": rk, "cdx": str(x), "cdy": str(y),
             "cdw": str(w), "cdi": str(col), "cdn": "24", "cdr": "0"}
        f.update(extra)
        return f
    good = [line(str(100 + i), min(i, 12 + (i % 12)), 900 - (i % 3) * 20) for i in range(30)]
    good = [(1.0 + i * 0.1, f) for i, f in enumerate(good)]
    keys = [(0.95 + i * 0.1, "right") for i in range(40)]
    found, fails = analyse_focus(good, keys, order, 1, 24)
    assert not fails, fails
    assert found["off_canvas"]["card_partial"] == 0 and found["max_cdn"] == 24 and found["last"] == 29
    # a teleport: same card, drawn 1200 px away in one frame (x), or 700 px in y
    bad = list(good) + [(5.0, dict(good[-1][1], cdx="-300"))]
    assert any("slot jump" in f and "in x" in f for f in analyse_focus(bad, keys, order, 1, 24)[1])
    bad = list(good) + [(5.0, dict(good[-1][1], cdy="1000"))]
    assert any("slot jump" in f and "in y" in f for f in analyse_focus(bad, keys, order, 1, 24)[1])
    # skipped card, focus without a key, window overrun
    skip = [good[0], (1.1, dict(good[1][1], rk="103", cdk="103"))]
    assert any("skipped" in f for f in analyse_focus(skip, keys, order, 1)[1])
    assert any("before any key" in f for f in analyse_focus(good, [(9.0, "right")], order, 1)[1])
    over = [(1.0, dict(good[0][1], cdn="25"))]
    assert any("window bound" in f for f in analyse_focus(over, keys, order, 1, 24)[1])
    # the Home fingerprint of before the card probe: `fx` and `col` only
    legacy = [(1.0, {"route": "home", "row": "1", "col": "2", "rk": "102", "fx": "640"}),
              (1.1, {"route": "home", "row": "1", "col": "3", "rk": "103", "fx": "900"})]
    found, fails = analyse_focus(legacy, keys, order, 1)
    assert not fails and found["off_canvas"]["frames"] == 0 and found["last"] == 3, (found, fails)
    # a grid: one key moves a row, the last short row is entered on its last card
    grid = [(1.0 + i * 0.1, line(str(100 + i), 0, 90, cdi="0")) for i in (42, 48, 54, 59)]
    assert not analyse_focus(grid, [(0.9, "down")] * 4, order, 6)[1]
    skipped = [grid[0], (1.1, line("109", 0, 90))]
    assert any("skipped" in f for f in analyse_focus(skipped, [(0.9, "down")] * 4, order, 6)[1])
    # off the canvas: partly, wholly, and the caption
    edge = [(1.0, line("100", 0, 1800, cdc="1800", cdcw="250")), (1.1, line("100", 0, 2004, cdc="2004", cdcw="100")),
            (1.2, line("100", 0, -315, cdc="-315", cdcw="100"))]
    off = analyse_focus(edge, keys, order, 1)[0]["off_canvas"]
    assert off["card_partial"] == 1 and off["card_off"] == 2 and off["caption_partial"] == 1 \
        and off["caption_off"] == 2 and off["min_x"] == -315 and off["max_right"] == 2274, off
    assert analyse_focus([(1.0, line("100", 0, 100, cdy="-343"))], keys, order, 1)[0]["off_canvas"]["card_off_y"] == 1
    # art regression total comes from the running counter
    reg = [(1.0, line("100", 0, 100, cdr="0")), (1.1, line("100", 0, 100, cdr="3"))]
    assert analyse_focus(reg, keys, order, 1)[0]["art_regressions"] == 3
    assert place_late_frames([10.0], [10.1, 12.0]) == (1, 1)
    assert parse_focus("focus route=home snapt=1 row=1 col=3 rk=44 fx=-12")["fx"] == "-12"
    # per-page figures: a page's posters are matched by the ratingKey in the transcode url
    rows = [{"class": "listing", "path": "/library/sections/1/all", "start": "12", "size": "12", "failed": False,
             "t_recv": 1.0, "t_done": 1.5},
            {"class": "image", "path": "/photo/:/transcode", "start": None, "size": None, "failed": False,
             "url": "/library/metadata/115/thumb/1", "t_recv": 1.6, "t_done": 2.0},
            {"class": "image", "path": "/photo/:/transcode", "start": None, "size": None, "failed": False,
             "url": "%2Flibrary%2Fmetadata%2F119%2Fthumb%2F1", "t_recv": 1.7, "t_done": 2.4},
            {"class": "image", "path": "/photo/:/transcode", "start": None, "size": None, "failed": False,
             "url": "/library/metadata/200/thumb/1", "t_recv": 1.7, "t_done": 9.0}]
    figures, landed = page_figures(rows, r"^/library/sections/1/all$", order)
    assert landed == [1.5] and figures[0]["ask_to_landed_ms"] == 500 and figures[0]["posters_asked_within_8s"] == 2 \
        and figures[0]["landed_to_last_of_its_posters_ms"] == 900, figures
    assert resolve_profile("lossy-503").error_kind == "503" and resolve_profile("none").error_rate == 0
    assert set(SURFACES) >= {"home-row", "library-grid", "search-row", "detail-related-row", "collection",
                             "long-season", "library-section-hub-row"}
    print("paging_link selftest: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--surface", action="append", help=f"one of {', '.join(SURFACES)} (default all)")
    ap.add_argument("--profile", action="append",
                    help=f"link profile (default {', '.join(DEFAULT_PROFILES)}); any of "
                         f"{', '.join(sorted(link_conditioner.PROFILES))}, {', '.join(EXTRA_PROFILES)}")
    ap.add_argument("--movies", type=int, default=None, help="override the surface's movie count")
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
            print(json.dumps(res, indent=1), flush=True)
            for f in fails:
                print(f"FAIL [{surface} / {profile}] {f}", flush=True)
            if args.json:
                args.json.write_text(json.dumps(results, indent=1))
    print(f"paging_link: {len(results)} runs, {bad} failing")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
