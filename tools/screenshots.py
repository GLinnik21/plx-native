#!/usr/bin/env python3
"""Regenerate the documentation screenshots: `make screenshots`.

Every figure is a named target STATE in `tests/screenshots/scenes.json`. For each scene this
driver:

  1. starts `tests/mock_pms.py` in catalog mode (the demo library, `tests/demo_library/`) on a
     free loopback port, in this process;
  2. makes a fresh instance root and arms the scene's triggers in it, plus three the driver owns:
     `token` (a placeholder the mock accepts), `plextv` (plex.tv replaced by the mock, so nothing
     leaves the machine) and `stillclock` (free-running animation held still);
  3. runs the simulator with `PLXNATIVE_SHOT_SETTLE`: the app itself writes one PNG once its
     screen has been at rest for the scene's settle time, and exits. The driver waits on that
     process — an artifact, never a sleep — under a ceiling timeout;
  4. checks the capture is the state the manifest names: every `expect` line is in the event log,
     no trigger was refused (`BADTRIGGER`) or gave up, and the mock saw no request it could not
     answer;
  5. scales and encodes each output with ffmpeg (Lanczos, `-bitexact`, one thread) into
     `docs/screenshots/` or `--out`.

`--check-determinism` captures every scene twice and compares the two PNGs pixel by pixel: every
channel of every pixel may differ by at most the scene's `max_delta` (default 1: the GPU's
rounding, which is not bit-stable run to run), except inside the scene's `free_regions`, each of
which must carry a `tolerance_reason`.

`--hero-variants` also renders the home scene once per film in the catalog's `hero` and
`hero_alternatives` as `home-hero-<film>.jpg`, for choosing the hero; `--hero` swaps the film the
home scenes pin for this run. Both write to the output directory like every other image.

Nothing here reads a gitignored file, and nothing touches a Plex account or a television.
"""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tests"))
import mock_pms  # noqa: E402

SCENES = ROOT / "tests" / "screenshots" / "scenes.json"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
# What the `token` trigger holds. The mock accepts any token; this one only has to be non-empty.
PLACEHOLDER_TOKEN = "demo-library-token"
# Lines that mean an arm did not reach its state, whatever else the log says.
REFUSALS = ("BADTRIGGER", "gave up")


def die(msg):
    print(f"screenshots: {msg}", file=sys.stderr)
    sys.exit(1)


def size_of(spec):
    w, h = spec.split("x")
    return int(w), int(h)


def clean_env(rt, shot, scene, defaults):
    """The simulator's environment: this machine's, minus every PLXNATIVE_* variable (a developer's
    own session settings must not leak into a published figure), plus the scene's."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("PLXNATIVE_")}
    env.update({
        "PLXNATIVE_RUNTIME_DIR": str(rt),
        "PLXNATIVE_APP_DIR": str(ROOT / "pkg"),
        "PLXNATIVE_WIN": defaults["canvas"],
        "PLXNATIVE_SHOT": str(shot),
        "PLXNATIVE_SHOT_EXIT": "1",
        "PLXNATIVE_SHOT_SETTLE": str(scene.get("settle_ms", defaults["settle_ms"])),
        "PLXNATIVE_SHOT_AFTER": str(scene.get("after_ms", defaults["after_ms"])),
    })
    return env


def capture(binary, scene, defaults, hero, keep):
    """Boot one scene and return (png bytes, log text). Raises RuntimeError on any failure."""
    srv, pms = mock_pms.serve(0, catalog=CATALOG, hero=hero)
    port = srv.server_address[1]
    rt = pathlib.Path(tempfile.mkdtemp(prefix=f"plxnative-shot-{scene['name']}-"))
    try:
        triggers = {"plextv": f"http://127.0.0.1:{port}", "stillclock": str(defaults["stillclock_ms"])}
        if not scene.get("no_token"):
            triggers["token"] = PLACEHOLDER_TOKEN
        triggers.update(scene.get("triggers", {}))
        for name, value in triggers.items():
            (rt / f"plxnative-{name}").write_text(value)
        shot = rt / "shot.png"
        out = open(rt / "sim.out", "w")
        timeout = scene.get("timeout_s", defaults["timeout_s"])
        t0 = time.monotonic()
        try:
            rc = subprocess.run([str(binary), "127.0.0.1", str(port)], env=clean_env(rt, shot, scene, defaults),
                                stdout=out, stderr=subprocess.STDOUT, timeout=timeout).returncode
        except subprocess.TimeoutExpired:
            rc = None
        finally:
            out.close()
        secs = time.monotonic() - t0
        log_path = rt / "plxnative-events.log"
        log = log_path.read_text(errors="replace") if log_path.exists() else ""
        problems = []
        if rc is None:
            problems.append(f"no settled frame within {timeout}s (the screen never came to rest)")
        elif rc != 0:
            problems.append(f"simulator exited {rc}")
        if not shot.exists():
            problems.append("no capture written")
        for line in scene.get("expect", []):
            if line not in log:
                problems.append(f"expected log line missing: {line!r}")
        for bad in REFUSALS:
            hits = [l for l in log.splitlines() if bad in l]
            if hits:
                problems.append(f"{bad}: {hits[0]}")
        if pms.unknown:
            problems.append(f"the mock could not answer: {sorted(set(pms.unknown))[:5]}")
        if problems:
            tail = "\n    ".join(log.splitlines()[-15:])
            raise RuntimeError("; ".join(problems) + f"\n  instance root: {rt}\n  log tail:\n    {tail}")
        png = shot.read_bytes()
        print(f"  {scene['name']}: settled in {secs:.1f}s")
        return png, log
    finally:
        srv.shutdown()
        srv.server_close()
        if not keep:
            shutil.rmtree(rt, ignore_errors=True)


def raw_rgb(png, w, h):
    """Decode a PNG to packed RGB24 through ffmpeg (no Python imaging dependency)."""
    return subprocess.run(["ffmpeg", "-v", "error", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "rgb24",
                           "-s", f"{w}x{h}", "pipe:1"], input=png, capture_output=True, check=True).stdout


def compare(a, b, w, h, free_regions):
    """(differing pixels, largest channel difference) outside `free_regions` ([x, y, w, h] each)."""
    ra, rb = raw_rgb(a, w, h), raw_rgb(b, w, h)
    if len(ra) != len(rb):
        return w * h, 255
    free = [(x, y, x + rw, y + rh) for x, y, rw, rh in free_regions]
    n = worst = 0
    for i in range(0, len(ra), 3):
        if ra[i:i + 3] == rb[i:i + 3]:
            continue
        p = i // 3
        x, y = p % w, p // w
        if any(x0 <= x < x1 and y0 <= y < y1 for x0, y0, x1, y1 in free):
            continue
        n += 1
        worst = max(worst, abs(ra[i] - rb[i]), abs(ra[i + 1] - rb[i + 1]), abs(ra[i + 2] - rb[i + 2]))
    return n, worst


def encode(png, dst, size):
    """PNG → JPEG at `size`, deterministically for a given ffmpeg build."""
    w, h = size_of(size)
    tmp = dst.with_suffix(".tmp.jpg")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-threads", "1", "-i", "pipe:0",
                    "-vf", f"scale={w}:{h}:flags=lanczos", "-frames:v", "1",
                    "-pix_fmt", "yuvj420p", "-q:v", "2", "-bitexact", "-map_metadata", "-1",
                    "-f", "mjpeg", str(tmp)], input=png, check=True)
    tmp.replace(dst)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", required=True, type=pathlib.Path, help="the simulator (make screenshots-sim)")
    ap.add_argument("--out", type=pathlib.Path, default=ROOT / "docs" / "screenshots")
    ap.add_argument("--only", help="comma-separated scene names or output files (home, ux-detail.jpg, …)")
    ap.add_argument("--check-determinism", action="store_true",
                    help="capture every scene twice and require the documented tolerance")
    ap.add_argument("--hero", help="the film the home scenes pin (default: the catalog's `hero`)")
    ap.add_argument("--hero-variants", action="store_true",
                    help="also render home-hero-<film>.jpg for the catalog's hero and its alternatives")
    ap.add_argument("--keep", action="store_true", help="keep each scene's instance root (for debugging)")
    a = ap.parse_args()

    if not a.bin.exists():
        die(f"{a.bin} does not exist — build it with `make screenshots-sim`")
    if shutil.which("ffmpeg") is None:
        die("ffmpeg is not on PATH")
    manifest = json.loads(SCENES.read_text())
    for s in manifest["scenes"]:
        if s.get("free_regions") and not s.get("tolerance_reason"):
            die(f"scene {s['name']}: free_regions without a tolerance_reason")
    defaults = dict(manifest["defaults"], canvas=manifest["canvas"])
    catalog = json.loads(CATALOG.read_text())
    try:
        mock_pms.CatalogLibrary(CATALOG)
    except ValueError as e:
        die(f"{e} — run `make demo-library`")
    canvas = size_of(manifest["canvas"])

    scenes = manifest["scenes"]
    if a.only:
        wanted = {w.strip().removesuffix(".jpg") for w in a.only.split(",") if w.strip()}
        scenes = [s for s in scenes
                  if s["name"] in wanted or any(o["file"].removesuffix(".jpg") in wanted for o in s["outputs"])]
        if not scenes:
            die(f"--only {a.only!r} names no scene")
    a.out.mkdir(parents=True, exist_ok=True)

    jobs = [(s, a.hero, [(o["file"], o["size"]) for o in s["outputs"]]) for s in scenes]
    if a.hero_variants:
        home = next(s for s in manifest["scenes"] if s.get("hero_variants"))
        size = home["outputs"][0]["size"]
        for film in [catalog["hero"], *catalog.get("hero_alternatives", [])]:
            jobs.append((dict(home, name=f"home-hero-{film}"), film, [(f"home-hero-{film}.jpg", size)]))

    failed, report = [], []
    for scene, hero, outputs in jobs:
        try:
            png, _ = capture(a.bin, scene, defaults, hero, a.keep)
            if a.check_determinism:
                again, _ = capture(a.bin, scene, defaults, hero, a.keep)
                bound = scene.get("max_delta", defaults["max_delta"])
                free = scene.get("free_regions", [])
                if again == png:
                    report.append(f"{scene['name']}: identical")
                else:
                    n, worst = compare(png, again, *canvas, free)
                    verdict = "within" if worst <= bound else "OVER"
                    where = f" outside {len(free)} free region(s)" if free else ""
                    report.append(f"{scene['name']}: {n} pixel(s) differ{where}, max |Δ| {worst} {verdict} {bound}")
                    if worst > bound:
                        raise RuntimeError(f"not deterministic: max |Δ| {worst} > {bound}{where}")
            for file, size in outputs:
                encode(png, a.out / file, size)
        except RuntimeError as e:
            failed.append(scene["name"])
            print(f"  {scene['name']}: FAILED — {e}", file=sys.stderr)
    for line in report:
        print(f"determinism  {line}")
    if failed:
        die(f"{len(failed)} scene(s) failed: {', '.join(failed)}")
    print(f"screenshots: {sum(len(o) for _, _, o in jobs)} image(s) written to {a.out}")


if __name__ == "__main__":
    main()
