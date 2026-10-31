#!/usr/bin/env python3
"""Snapshot the glass materials over SYNTHETIC grounds, not over whatever poster is on screen.

Every material question in this app is a question about the bar over a ground, and until this
existed the ground was the hero's artwork: it advances on its own clock, two simulator instances
launched together drift apart within seconds, and a comparison assembled by matching log lines can
silently pair two different pictures. All three produced wrong answers during the tab-track work.

So this drives `ui::testpat` instead. A scripted ladder of grounds through the `pat:` remote token,
a shot at each rung, and a contact sheet out the other end:

    tools/glass-patterns.py                       # the default ladder
    tools/glass-patterns.py --pats flat:20,flat:60,edge,checker:24
    tools/glass-patterns.py --out /tmp/before   # …then again after a change, and diff the sheets
    tools/glass-patterns.py --lens "off:2,0,0 a:10,10,0 b:14,18,0"   # …one instance per rung

`--lens` is the second axis: each entry is `<name>:<bevel>,<lens>,<spec>`, written to that
instance's `plxnative-tracklens`, so a run photographs every geometry against every ground and the
per-ground sheets stack the rungs for comparison. It is read once per process, which is why a rung
costs a whole simulator — and why they are launched together and driven in lockstep.

Sheets land in the output directory (default a scratch dir it prints), plus a table of what the bar
actually DREW at each rung: the last `track_ground` line the bar logged on that rung (L*, span, the
density it wanted and the one it drew, where its ground came from) and the contrast the idle labels
MEASURE in the rung's screenshot against the plate under them (median and worst of it). The solver's belief (`want`) and the
pixels are different things, and "does it look right" and "does it hold the floor" are different
questions; the table answers both. It is printed, and written beside the sheets as `table.txt`.

Requires a built simulator (`make sim`) and a PMS to boot against. By default the host, port and
token come from src/config.local.h, same as `make sim-run`; `--pms HOST --port N --token T` name
them on the command line instead, and `--mock` additionally points plex.tv and Discover at that
host (so a run against `tests/mock_pms.py` reaches nothing else). `--repo` and `--sim` let the tool
run from a copy outside the checkout, against a simulator binary copied aside, which is how two
builds are compared on one ladder.
"""
import argparse, os, re, shutil, signal, subprocess, sys, time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SIM = REPO / "rust-modules/target-sim/debug/plxnative-sim"
# The tab track, in authored 1920x1080 coordinates, when no `track_ground` line has said where it
# is: the bar logs its rect as `rect=` when `plxnative-groundlog` is armed, and the crops and the
# plate measurement use that, since the track's width follows the strip's labels.
TRACK = (686, 36, 547, 76)
# The token a booted `--mock` run is given: the mock accepts any non-empty one.
MOCK_TOKEN = "demo-library-token"
# The contrast the bar solves for (`TRACK_INK_CONTRAST` in `ui::widgets`) — the floor the table's
# measured column is read against. Printed beside it, never used to pass or fail a rung.
FLOOR = 4.0
LADDER = ([f"flat:{l}" for l in range(0, 101, 10)]
          + ["ramp", "edge", "checker:24", "lines:6"]
          # colour: the bare sweep for what a poster actually looks like, then the same sweep pinned
          # to three lightnesses — those are the ones that answer whether the solve is even-handed
          # across HUE, because everything else about the ground is held still
          + ["rainbow", "rainbow:35", "rainbow:80"]
          # hue held one at a time at a fixed lightness — `solid` is to hue what `flat` is to
          # lightness, and the only form the bar's five-tap average cannot flatten
          + [f"solid:{d}:60" for d in range(0, 360, 45)])
# The four ink packages this tool was built to compare are gone: a judging panel picked one and the
# other three were deleted with the second polarity they existed to argue about. What is left is a
# ladder of grounds against ONE material, which is what a snapshot is for — the comparison this now
# serves is against the last run, not against a variant.
PACKAGES = ["current"]


def cfg(name):
    m = re.search(rf'^#define\s+{name}\s+"([^"]*)"', (REPO / "src/config.local.h").read_text(), re.M)
    return m.group(1) if m else None


def cfg_port():
    m = re.search(r"^#define\s+PMS_PORT\s+(\d+)", (REPO / "src/config.local.h").read_text(), re.M)
    return m.group(1) if m else "32400"


def launch(root: Path, pkg: str, pms: str, port: str, token: str, mock: bool = False,
           lens: str = "", lift: str = "", sharp: str = ""):
    root.mkdir(parents=True, exist_ok=True)
    # the container's lens geometry, one rung per instance — `gfx::standing_sweep` reads it once
    if lens:
        (root / "plxnative-tracklens").write_text(lens)
    # …and its diffuse floor, the same way; "0" is the material before the floor existed
    if lift:
        (root / "plxnative-tracklift").write_text(lift)
    # …and how much of the rim shows the page unblurred
    if sharp:
        (root / "plxnative-tracksharp").write_text(sharp)
    if token:
        (root / "plxnative-token").write_text(token)
    if mock:
        # a token boot has no account: plex.tv and Discover are pointed at the same mock, so the
        # run reaches no host but the one it was given
        (root / "plxnative-plextv").write_text(f"http://{pms}:{port}")
        (root / "plxnative-discover").write_text(f"http://{pms}:{port}")
    # `noidle` because a settled screen stops presenting and `shot` would then wait for a frame that
    # never comes; `groundlog` because the numbers below are read from it.
    (root / "plxnative-noidle").touch()
    (root / "plxnative-groundlog").touch()
    (root / "plxnative-testpat").write_text("flat:0")
    env = dict(os.environ, PLXNATIVE_RUNTIME_DIR=str(root), PLXNATIVE_APP_DIR=str(REPO / "pkg"),
               PLXNATIVE_WIN="1920x1080")
    log = open(root / "stdout.log", "w")
    return subprocess.Popen([str(SIM), pms, port], env=env, stdout=log, stderr=log)


def drive(root: Path, pats, settle):
    """Walk the ladder, one shot per rung, and return [(pat, shot_path)] in order."""
    fifo = root / "plxnative-remote"
    for _ in range(120):
        if fifo.exists():
            break
        time.sleep(0.25)
    else:
        raise SystemExit(f"no FIFO in {root} — did the app start? see stdout.log")
    out = []
    with open(fifo, "r+b", buffering=0) as f:
        for i, pat in enumerate(pats):
            f.write(f"pat:{pat} ".encode())
            # the ground is sampled every ~0.5 s of drawn frames and the weight then springs to it,
            # so a rung needs real time before it is worth photographing
            time.sleep(settle)
            f.write(b"shot ")
            time.sleep(0.6)
            out.append((pat, root / f"shot-{i + 1}.png"))
    return out


# What `draw_tab_row` logs when `plxnative-groundlog` is armed:
#   track_ground rgb=R,G,B L*=… span=… want=… drawn=… rect=x,y,w,h src=…
# `src=` is newer than the rest (builds before it end at `rect=`), and `span=` is optional too, so a
# line from an older build still parses. The end is left open: a field appended after `src=` must not
# break the parse.
GROUND = re.compile(
    r"track_ground rgb=(?P<r>[\d.]+),(?P<g>[\d.]+),(?P<b>[\d.]+) L\*=(?P<lstar>-?[\d.]+)"
    r"(?: span=(?P<span>-?[\d.]+))? want=(?P<want>[\d.]+) drawn=(?P<drawn>[\d.]+)"
    r"(?: rect=(?P<rect>[\d.]+,[\d.]+,[\d.]+,[\d.]+))?(?: src=(?P<src>\w+))?")


def readings(root: Path):
    """Every `track_ground` line in the instance's event log, in order, as dicts of its fields."""
    seen = []
    for line in (root / "plxnative-events.log").read_text(errors="replace").splitlines():
        m = GROUND.search(line)
        if m:
            d = m.groupdict()
            seen.append({
                "lstar": float(d["lstar"]),
                "span": float(d["span"]) if d["span"] else None,
                "want": float(d["want"]), "drawn": float(d["drawn"]),
                "rect": tuple(float(v) for v in d["rect"].split(",")) if d["rect"] else None,
                "src": d["src"] or "-",
            })
    return seen


def settled(r):
    """Has the drawn density arrived at the one the ground asked for? It eases there, so a line read
    while the bar is still travelling says nothing about the ground."""
    return abs(r["drawn"] - r["want"]) < 0.005


def lum(c):
    """WCAG relative luminance of an 8-bit sRGB triple."""
    def lin(v):
        v /= 255.0
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    return 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])


# A pixel is label ink when its relative luminance reaches this: every ink in the theme is a light
# cool grey (the idle labels' `TEXT_READING` measures 0.65), and a plate the bar has solved to its
# floor sits at 0.12 or below, so the line between them has room on both sides. It is how a label is
# FOUND; its contrast is measured against the plate under it.
INK_LUM = 0.30
# …and a pixel is plate (not ink, not the fringe antialiasing leaves around it) below this.
PLATE_LUM = 0.25


def label_contrast(shot: Path, rect):
    """The contrast each label in the tab track MEASURES in a screenshot, left to right, as
    (median, worst, x0, x1): contrasts in the sense of WCAG, x in authored px.

    A label is a run of columns holding ink pixels (see `INK_LUM`) in the track's middle rows, with
    runs more than 18 px apart kept apart. Its ink is the mean of the brightest 2% of the pixels in
    those columns (the glyph bodies, which reach full ink). The plate under it is the pixels of the
    same box that are neither ink nor its fringe (see `PLATE_LUM`): `median` is the contrast against
    their median, `worst` against their 90th percentile, which is what the label is hardest to read
    against where the ground varies under it (a checker, an edge). On a flat ground the two agree.
    The first run is the selected pill and is returned like the rest; the caller decides which are
    idle."""
    from PIL import Image
    im = Image.open(shot).convert("RGB")
    k = im.width / 1920.0
    x, y, w, h = rect
    # the track's rounded ends carry the rim's highlight, which is lighter than ink on a bright
    # ground and is no label: the search starts 20 px inside them (the first pill is 28 px in)
    x0, x1 = int((x + 20) * k), int((x + w - 20) * k)
    mid = range(int((y + h * 0.3) * k), int((y + h * 0.7) * k))
    px = im.load()
    cols = list(range(x0, x1))
    lums = {cx: [lum(px[cx, cy]) for cy in mid] for cx in cols}
    lit = [cx for cx in cols if max(lums[cx]) >= INK_LUM]
    runs, cur = [], []
    for cx in lit:
        if cur and cx - cur[-1] > 18 * k:
            runs.append(cur)
            cur = []
        cur.append(cx)
    if cur:
        runs.append(cur)
    out = []
    for run in runs:
        if len(run) < 6 * k:
            continue
        body = sorted((v for cx in run for v in lums[cx]), reverse=True)
        n = max(1, len(body) // 50)
        ink = sum(body[:n]) / n
        under = sorted(v for cx in run for v in lums[cx] if v < PLATE_LUM)
        if not under:
            continue
        med, worst = under[len(under) // 2], under[min(len(under) - 1, int(len(under) * 0.9))]
        out.append(((ink + 0.05) / (med + 0.05), (ink + 0.05) / (worst + 0.05), run[0] / k, run[-1] / k))
    return out


def idle_labels(runs):
    """The idle TEXT labels among a track's runs: every run but the first (the selected pill), and
    not the icons (a run narrower than 60 authored px)."""
    return [r for r in runs[1:] if r[3] - r[2] >= 60]


def table(pkgs, rungs, shots):
    """One row per (ground, build): the last line logged on the rung, whether the bar had arrived,
    how many of the settled lines among the rung's had `src=image`, and the measured contrast of
    the idle text labels (see [`idle_labels`])."""
    rows = [f"{'ground':<12} {'build':<8} {'L*':>5} {'span':>5} {'want':>6} {'drawn':>6} "
            f"{'src':<6} {'arrived':<7} {'settled-lines':>13} {f'idle contrast med/worst (floor {FLOOR:.1f})':>36}"]
    for i, (pat, _) in enumerate(rungs[pkgs[0]]):
        for pkg in pkgs:
            rs = rungs[pkg][i][1]
            if not rs:
                rows.append(f"{pat:<12} {pkg:<8} {'(no track_ground line on this rung)'}")
                continue
            last = rs[-1]
            ok = [r for r in rs if settled(r)]
            img = sum(1 for r in ok if r["src"] == "image")
            # a build before `src=` prints no word, and "0 of 7 came from the image" would be a lie
            seen = f"{img}/{len(ok)}" if any(r["src"] != "-" for r in rs) else "n/a"
            span = f"{last['span']:.1f}" if last["span"] is not None else "-"
            shot = shots[pkg][i][1]
            try:
                labels = idle_labels(label_contrast(shot, last["rect"] or TRACK)) if shot.exists() else []
            except Exception as e:  # a missing PIL or a bad crop must not lose the log columns
                labels = []
                print(f"  (contrast not measured for {pat}/{pkg}: {e})", file=sys.stderr)
            idle = " ".join(f"{med:.2f}/{worst:.2f}" for med, worst, _, _ in labels) or "-"
            rows.append(f"{pat:<12} {pkg:<8} {last['lstar']:>5.1f} {span:>5} {last['want']:>6.3f} "
                        f"{last['drawn']:>6.3f} {last['src']:<6} {'yes' if settled(last) else 'NO':<7} "
                        f"{seen:>13} {idle:>36}")
    return "\n".join(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="", help="checkout holding pkg/ and src/config.local.h (default: this tool's)")
    ap.add_argument("--sim", default="", help="simulator binary (default: the checkout's target-sim build)")
    ap.add_argument("--pms", default="", help="PMS host to boot against (default: PMS_HOST in src/config.local.h)")
    ap.add_argument("--port", default="", help="its port (default: PMS_PORT there, else 32400)")
    ap.add_argument("--token", default="", help="the token to boot with (default: PMS_TOKEN there)")
    ap.add_argument("--mock", action="store_true",
                    help="the PMS is tests/mock_pms.py: boot with a placeholder token and point plex.tv and Discover at it")
    ap.add_argument("--packages", default=",".join(PACKAGES))
    ap.add_argument("--lens", default="", help='space-separated <name>:<bevel>,<lens>,<spec> rungs')
    ap.add_argument("--lift", default="", help='space-separated <name>:<floor> rungs (0 = no floor)')
    ap.add_argument("--sharp", default="", help='space-separated <name>:<weight> rungs (0 = one source)')
    ap.add_argument("--pats", default=",".join(LADDER))
    ap.add_argument("--settle", type=float, default=2.5, help="seconds to let a rung settle")
    ap.add_argument("--out", default="")
    a = ap.parse_args()

    global REPO, SIM
    if a.repo:
        REPO = Path(a.repo).resolve()
        SIM = REPO / "rust-modules/target-sim/debug/plxnative-sim"
    if a.sim:
        SIM = Path(a.sim).resolve()
    if not SIM.exists():
        raise SystemExit("no simulator — run `make sim` first")
    # the command line names the PMS, or src/config.local.h does; a run that names it needs no file
    pms = a.pms or cfg("PMS_HOST")
    port = a.port or (cfg_port() if not a.pms else "32400")
    token = a.token or (MOCK_TOKEN if a.mock else cfg("PMS_TOKEN") if not a.pms else "")
    if not pms:
        raise SystemExit("no PMS: pass --pms, or set PMS_HOST in src/config.local.h")

    out = Path(a.out) if a.out else Path("/tmp/glass-patterns")
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    pats = [p for p in a.pats.split(",") if p]
    lens = dict(t.split(":", 1) for t in a.lens.split() if t)
    lift = dict(t.split(":", 1) for t in a.lift.split() if t)
    sharp = dict(t.split(":", 1) for t in a.sharp.split() if t)
    pkgs = list(lens) or list(lift) or list(sharp) or [p for p in a.packages.split(",") if p]

    procs, roots = {}, {}
    for pkg in pkgs:
        roots[pkg] = out / pkg
        procs[pkg] = launch(roots[pkg], pkg, pms, port, token, a.mock,
                            lens.get(pkg, ""), lift.get(pkg, ""), sharp.get(pkg, ""))
    print(f"launched {len(pkgs)} simulators; ladder of {len(pats)} grounds, {a.settle}s each "
          f"(~{len(pats) * (a.settle + 0.6) + 8:.0f}s)")
    time.sleep(8)

    shots, rungs = {}, {}
    try:
        # driven in lockstep so every package sees the same ground at the same moment
        fifos = {pkg: open(roots[pkg] / "plxnative-remote", "r+b", buffering=0) for pkg in pkgs}
        for pkg in pkgs:
            shots[pkg], rungs[pkg] = [], []
        for i, pat in enumerate(pats):
            # the lines already logged are the PREVIOUS rung's: this rung's are the ones after them
            seen = {pkg: len(readings(roots[pkg])) for pkg in pkgs}
            for f in fifos.values():
                f.write(f"pat:{pat} ".encode())
            time.sleep(a.settle)
            for f in fifos.values():
                f.write(b"shot ")
            time.sleep(0.6)
            for pkg in pkgs:
                shots[pkg].append((pat, roots[pkg] / f"shot-{i + 1}.png"))
                rungs[pkg].append((pat, readings(roots[pkg])[seen[pkg]:]))
            print(f"  {pat}")
        for f in fifos.values():
            f.close()
    finally:
        for p in procs.values():
            p.send_signal(signal.SIGTERM)
        time.sleep(1)

    text = table(pkgs, rungs, shots)
    (out / "table.txt").write_text(text + "\n")
    print("\n" + text)
    build_sheets(out, pkgs, pats, shots)


def build_sheets(out, pkgs, pats, shots):
    from PIL import Image, ImageDraw
    x, y, w, h = TRACK
    pad = 90
    cx, cy, cw, ch = max(0, x - pad), max(0, y - pad), w + pad * 2, h + pad * 2

    # one sheet per ground, packages stacked — the comparison that matters
    for i, pat in enumerate(pats):
        rows = []
        for pkg in pkgs:
            p = shots[pkg][i][1]
            if p.exists():
                rows.append((pkg, Image.open(p).convert("RGB").crop((cx, cy, cx + cw, cy + ch))))
        if not rows:
            continue
        sheet = Image.new("RGB", (cw, (ch + 18) * len(rows)), (16, 16, 16))
        d = ImageDraw.Draw(sheet)
        for j, (pkg, im) in enumerate(rows):
            sheet.paste(im, (0, j * (ch + 18) + 18))
            d.text((6, j * (ch + 18) + 4), pkg, fill=(255, 255, 255))
        sheet.save(out / f"ground-{pat.replace(':', '')}.png")

    # and one contact sheet per package, the whole ladder
    for pkg in pkgs:
        rows = [(pat, Image.open(p).convert("RGB").crop((cx, cy, cx + cw, cy + ch)))
                for pat, p in shots[pkg] if p.exists()]
        if not rows:
            continue
        sheet = Image.new("RGB", (cw, (ch + 18) * len(rows)), (16, 16, 16))
        d = ImageDraw.Draw(sheet)
        for j, (pat, im) in enumerate(rows):
            sheet.paste(im, (0, j * (ch + 18) + 18))
            d.text((6, j * (ch + 18) + 4), pat, fill=(255, 255, 255))
        sheet.save(out / f"ladder-{pkg}.png")
    print(f"\nsheets in {out}")


if __name__ == "__main__":
    main()
