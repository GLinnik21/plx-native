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
actually DREW at each rung — the plate's measured colour and the idle label's contrast over it —
because "does it look right" and "does it hold 3:1" are different questions and this answers both.

Requires a built simulator (`make sim`) and a PMS host in src/config.local.h, same as `make sim-run`.
"""
import argparse, os, re, shutil, signal, subprocess, sys, time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SIM = REPO / "rust-modules/target-sim/debug/plxnative-sim"
# The tab track, in authored 1920x1080 coordinates — `draw_tab_row` logs this rect as `rect=` when
# `plxnative-groundlog` is armed, and these are the values it prints. Used for the crops and for
# sampling the drawn plate.
TRACK = (686, 36, 547, 76)
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


def launch(root: Path, pkg: str, pms: str, port: str, lens: str = "", lift: str = ""):
    root.mkdir(parents=True, exist_ok=True)
    # the container's lens geometry, one rung per instance — `gfx::standing_sweep` reads it once
    if lens:
        (root / "plxnative-tracklens").write_text(lens)
    # …and its diffuse floor, the same way; "0" is the material before the floor existed
    if lift:
        (root / "plxnative-tracklift").write_text(lift)
    tok = re.search(r'^#define\s+PMS_TOKEN\s+"([^"]*)"', (REPO / "src/config.local.h").read_text(), re.M)
    if tok:
        (root / "plxnative-token").write_text(tok.group(1))
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


def drawn(root: Path):
    """The last `track_ground` line's (L*, tone, density) — what the bar settled on."""
    pat = re.compile(r"track_ground rgb=([\d.]+),([\d.]+),([\d.]+) L\*=([\d.-]+) "
                     r"tone=([\d.]+)->(\d) want=([\d.]+) drawn=([\d.]+)")
    seen = []
    for line in (root / "plxnative-events.log").read_text(errors="replace").splitlines():
        m = pat.search(line)
        if m:
            seen.append(tuple(float(x) for x in m.groups()))
    return seen


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--packages", default=",".join(PACKAGES))
    ap.add_argument("--lens", default="", help='space-separated <name>:<bevel>,<lens>,<spec> rungs')
    ap.add_argument("--lift", default="", help='space-separated <name>:<floor> rungs (0 = no floor)')
    ap.add_argument("--pats", default=",".join(LADDER))
    ap.add_argument("--settle", type=float, default=2.5, help="seconds to let a rung settle")
    ap.add_argument("--out", default="")
    a = ap.parse_args()

    if not SIM.exists():
        raise SystemExit("no simulator — run `make sim` first")
    pms, port = cfg("PMS_HOST"), None
    m = re.search(r"^#define\s+PMS_PORT\s+(\d+)", (REPO / "src/config.local.h").read_text(), re.M)
    port = m.group(1) if m else "32400"
    if not pms:
        raise SystemExit("no PMS_HOST in src/config.local.h")

    out = Path(a.out) if a.out else Path("/tmp/glass-patterns")
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    pats = [p for p in a.pats.split(",") if p]
    lens = dict(t.split(":", 1) for t in a.lens.split() if t)
    lift = dict(t.split(":", 1) for t in a.lift.split() if t)
    pkgs = list(lens) or list(lift) or [p for p in a.packages.split(",") if p]

    procs, roots = {}, {}
    for pkg in pkgs:
        roots[pkg] = out / pkg
        procs[pkg] = launch(roots[pkg], pkg, pms, port, lens.get(pkg, ""), lift.get(pkg, ""))
    print(f"launched {len(pkgs)} simulators; ladder of {len(pats)} grounds, {a.settle}s each "
          f"(~{len(pats) * (a.settle + 0.6) + 8:.0f}s)")
    time.sleep(8)

    shots = {}
    try:
        # driven in lockstep so every package sees the same ground at the same moment
        fifos = {pkg: open(roots[pkg] / "plxnative-remote", "r+b", buffering=0) for pkg in pkgs}
        for pkg in pkgs:
            shots[pkg] = []
        for i, pat in enumerate(pats):
            for f in fifos.values():
                f.write(f"pat:{pat} ".encode())
            time.sleep(a.settle)
            for f in fifos.values():
                f.write(b"shot ")
            time.sleep(0.6)
            for pkg in pkgs:
                shots[pkg].append((pat, roots[pkg] / f"shot-{i + 1}.png"))
            print(f"  {pat}")
        for f in fifos.values():
            f.close()
    finally:
        for p in procs.values():
            p.send_signal(signal.SIGTERM)
        time.sleep(1)

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
