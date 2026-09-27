#!/usr/bin/env python3
"""Regenerate the documentation screenshots and the website's close-up stills: `make screenshots`.

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
  5. crops, scales and encodes each output with ffmpeg (Lanczos, `-bitexact`, one thread) into a
     staging directory.

A scene may render SUPERSAMPLED: `render_scale` (1..4, default 1) sets `PLXNATIVE_RENDER_SCALE`,
so the simulator draws the 1920x1080 canvas at that multiple (glyphs, icons and artwork rasterised
to match) and the capture comes out that many times larger. An output may take a `crop`
([x, y, w, h] in CANVAS coordinates, so a crop means the same UI whatever the scale) and a JPEG
`quality` (ffmpeg's -q:v, default 2); its `size` defaults to the crop times the scale, and any
other size is a Lanczos resample. `dest` picks where it lands: `docs` (the default,
`docs/screenshots/`) or `site` (`site/media/`, the website's close-up stills); `--out` sends
every output to one directory instead. A `card` output is not cut from the capture at all: it is
the website's link-preview card (`site/og/card.html`), composed by `tools/render-og-card.sh`
around another output of the same scene once that one is staged, so it needs a headless Chromium
(the script says which it looks for) whenever its scene is in the run.

Only when every scene has succeeded is the staged set moved into place, together with a
`CREDITS.md` for the documentation figures written from the demo library's manifests
(`tools/demo_library.py`). So the one command that regenerates the images regenerates their
credits, the two cannot drift apart, and a failed run leaves every directory as it was.

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
import importlib.util
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

# tools/demo_library.py shares its name with the tests/demo_library package, so it is loaded by
# path under another name rather than through sys.path.
_spec = importlib.util.spec_from_file_location("demo_library_tool", ROOT / "tools" / "demo_library.py")
demo_library = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(demo_library)

SCENES = ROOT / "tests" / "screenshots" / "scenes.json"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
# What the `token` trigger holds. The mock accepts any token; this one only has to be non-empty.
PLACEHOLDER_TOKEN = "demo-library-token"
# Where an output's `dest` puts it when there is no --out.
DESTS = {"docs": ROOT / "docs" / "screenshots", "site": ROOT / "site" / "media"}
# What composes a `card` output (the site's link-preview card) around a staged figure.
CARD_SCRIPT = ROOT / "tools" / "render-og-card.sh"
# The supersampling factors the simulator accepts (`surface::render_scale`).
RENDER_SCALES = range(1, 5)
# Lines that mean an arm did not reach its state, whatever else the log says.
REFUSALS = ("BADTRIGGER", "gave up")


def die(msg):
    print(f"screenshots: {msg}", file=sys.stderr)
    sys.exit(1)


def size_of(spec):
    w, h = spec.split("x")
    return int(w), int(h)


def render_scale(scene):
    """The scene's `render_scale`, checked: an integer the simulator accepts."""
    n = scene.get("render_scale", 1)
    if not isinstance(n, int) or isinstance(n, bool) or n not in RENDER_SCALES:
        raise ValueError(f"scene {scene['name']}: render_scale {n!r} is not an integer in 1..4")
    return n


def output_spec(scene, out, canvas):
    """One output of `scene`, resolved: `(dest, file, crop, size, quality)`. `crop` is the pixel
    rectangle `(x, y, w, h)` of the scaled capture (canvas coordinates times the scene's
    `render_scale`, rounded); `size` is `(w, h)`, the crop's own size unless the output names
    another. Raises ValueError for anything the driver could not honour."""
    n = render_scale(scene)
    cw, ch = canvas
    where = f"scene {scene['name']}, output {out.get('file')!r}"
    dest = out.get("dest", "docs")
    if dest not in DESTS:
        raise ValueError(f"{where}: dest {dest!r} is not one of {sorted(DESTS)}")
    if "/" in out["file"] or not out["file"].endswith(".jpg"):
        raise ValueError(f"{where}: file must be a bare .jpg name")
    x, y, w, h = out.get("crop", [0, 0, cw, ch])
    if not all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in (x, y, w, h)):
        raise ValueError(f"{where}: crop must be four numbers")
    if w <= 0 or h <= 0 or x < 0 or y < 0 or x + w > cw or y + h > ch:
        raise ValueError(f"{where}: crop {[x, y, w, h]} is not inside the {cw}x{ch} canvas")
    px, py = round(x * n), round(y * n)
    crop = (px, py, round((x + w) * n) - px, round((y + h) * n) - py)
    size = size_of(out["size"]) if "size" in out else crop[2:]
    # A resample may change the scale, never the shape: 1% is the rounding of an integer size.
    if abs((size[0] / size[1]) / (crop[2] / crop[3]) - 1) > 0.01:
        raise ValueError(f"{where}: size {size[0]}x{size[1]} would stretch the {crop[2]}x{crop[3]} crop")
    quality = out.get("quality", 2)
    if not isinstance(quality, int) or isinstance(quality, bool) or not 1 <= quality <= 31:
        raise ValueError(f"{where}: quality {quality!r} is not ffmpeg's -q:v 1..31")
    return dest, out["file"], crop, size, quality


def card_spec(scene, out):
    """A CARD output — `{"file", "dest", "card": <another output of this scene>}` — resolved:
    `(dest, file, source)`. It is not cut from the capture: `tools/render-og-card.sh` composes
    the site's link-preview card around the staged `source` image, so the card always shows the
    figure this run made. Raises ValueError for anything the driver could not honour."""
    where = f"scene {scene['name']}, output {out.get('file')!r}"
    dest = out.get("dest", "docs")
    if dest not in DESTS:
        raise ValueError(f"{where}: dest {dest!r} is not one of {sorted(DESTS)}")
    if "/" in out["file"] or not out["file"].endswith(".jpg"):
        raise ValueError(f"{where}: file must be a bare .jpg name")
    if set(out) - {"file", "dest", "card"}:
        raise ValueError(f"{where}: a card takes no {sorted(set(out) - {'file', 'dest', 'card'})}")
    sources = [o for o in scene["outputs"] if o["file"] == out["card"] and "card" not in o]
    if not sources:
        raise ValueError(f"{where}: card {out['card']!r} is not another output of this scene")
    return dest, out["file"], sources[0]


def resolve_output(scene, out, canvas):
    """`card_spec` for a card output, `output_spec` for every other."""
    return card_spec(scene, out) if "card" in out else output_spec(scene, out, canvas)


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
    if render_scale(scene) > 1:
        env["PLXNATIVE_RENDER_SCALE"] = str(render_scale(scene))
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


def encode(png, dst, crop, size, quality=2):
    """PNG → JPEG: the `crop` rectangle `(x, y, w, h)` of the capture, at `size` `(w, h)` and
    ffmpeg JPEG quality `quality`, deterministically for a given ffmpeg build."""
    x, y, cw, ch = crop
    w, h = size
    chain = f"crop={cw}:{ch}:{x}:{y}"
    if (w, h) != (cw, ch):
        chain += f",scale={w}:{h}:flags=lanczos"
    tmp = dst.with_suffix(".tmp.jpg")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-threads", "1", "-i", "pipe:0",
                    "-vf", chain, "-frames:v", "1",
                    "-pix_fmt", "yuvj420p", "-q:v", str(quality), "-bitexact", "-map_metadata", "-1",
                    "-f", "mjpeg", str(tmp)], input=png, check=True)
    tmp.replace(dst)


def write_credits(out):
    """CREDITS.md for the images in `out`, from the manifests they were rendered from."""
    assets, catalog = demo_library.load()
    demo_library.check(assets, catalog)
    demo_library.credits(assets, catalog, out / "CREDITS.md")


def render_set(jobs, render, dests, keep=False):
    """Run `render(scene, hero, outputs, stage)` for every job, each output into `stage/<dest>/`,
    and move the whole set into `dests[<dest>]` only when every job succeeded, with a CREDITS.md
    beside the `docs` figures. Any exception fails its scene (a capture refused, ffmpeg exiting
    non-zero, the mock not starting); the other scenes still run, so one run reports every
    failure. Returns the failed scene names; when there are any, no destination has been touched,
    so a failed run never leaves a half-regenerated set."""
    stage = pathlib.Path(tempfile.mkdtemp(prefix="plxnative-shots-"))
    for dest in dests:
        (stage / dest).mkdir()
    failed = []
    try:
        for scene, hero, outputs in jobs:
            try:
                render(scene, hero, outputs, stage)
            except Exception as e:  # noqa: BLE001 — every failure is the scene's, reported below
                failed.append(scene["name"])
                why = e if isinstance(e, RuntimeError) else f"{type(e).__name__}: {e}"
                print(f"  {scene['name']}: FAILED — {why}", file=sys.stderr)
        if failed:
            if keep:
                print(f"  staged output kept in {stage}", file=sys.stderr)
            return failed
        write_credits(stage / "docs")
        for dest, out in dests.items():
            out.mkdir(parents=True, exist_ok=True)
            for f in sorted((stage / dest).iterdir()):
                shutil.move(str(f), str(out / f.name))
        return []
    finally:
        if not (keep and failed):
            shutil.rmtree(stage, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", required=True, type=pathlib.Path, help="the simulator (make screenshots-sim)")
    ap.add_argument("--out", type=pathlib.Path,
                    help="write every output here instead of docs/screenshots/ and site/media/")
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
    try:
        for s in manifest["scenes"]:
            for o in s["outputs"]:
                resolve_output(s, o, canvas)
    except ValueError as e:
        die(str(e))
    dests = {d: a.out for d in DESTS} if a.out else dict(DESTS)

    scenes = manifest["scenes"]
    if a.only:
        wanted = {w.strip().removesuffix(".jpg") for w in a.only.split(",") if w.strip()}
        scenes = [s for s in scenes
                  if s["name"] in wanted or any(o["file"].removesuffix(".jpg") in wanted for o in s["outputs"])]
        if not scenes:
            die(f"--only {a.only!r} names no scene")
    jobs = [(s, a.hero, s["outputs"]) for s in scenes]
    if a.hero_variants:
        home = next(s for s in manifest["scenes"] if s.get("hero_variants"))
        size = home["outputs"][0]["size"]
        for film in [catalog["hero"], *catalog.get("hero_alternatives", [])]:
            jobs.append((dict(home, name=f"home-hero-{film}"), film,
                         [{"file": f"home-hero-{film}.jpg", "size": size}]))

    report = []

    def render(scene, hero, outputs, stage):
        png, _ = capture(a.bin, scene, defaults, hero, a.keep)
        if a.check_determinism:
            again, _ = capture(a.bin, scene, defaults, hero, a.keep)
            bound = scene.get("max_delta", defaults["max_delta"])
            free = scene.get("free_regions", [])
            if again == png:
                report.append(f"{scene['name']}: identical")
            else:
                k = render_scale(scene)
                n, worst = compare(png, again, canvas[0] * k, canvas[1] * k,
                                   [[v * k for v in r] for r in free])
                verdict = "within" if worst <= bound else "OVER"
                where = f" outside {len(free)} free region(s)" if free else ""
                report.append(f"{scene['name']}: {n} pixel(s) differ{where}, max |Δ| {worst} {verdict} {bound}")
                if worst > bound:
                    raise RuntimeError(f"not deterministic: max |Δ| {worst} > {bound}{where}")
        for o in outputs:
            if "card" not in o:
                dest, file, crop, size, quality = output_spec(scene, o, canvas)
                encode(png, stage / dest / file, crop, size, quality)
        for o in outputs:
            if "card" in o:
                dest, file, source = card_spec(scene, o)
                subprocess.run(["bash", str(CARD_SCRIPT), "--shot",
                                str(stage / source.get("dest", "docs") / source["file"]),
                                "--out", str(stage / dest / file)], check=True, stdout=subprocess.DEVNULL)

    failed = render_set(jobs, render, dests, a.keep)
    for line in report:
        print(f"determinism  {line}")
    if failed:
        die(f"{len(failed)} scene(s) failed: {', '.join(failed)}; nothing was written")
    where = ", ".join(sorted({str(d) for d in dests.values()}))
    print(f"screenshots: {sum(len(o) for _, _, o in jobs)} image(s) and CREDITS.md written to {where}")


if __name__ == "__main__":
    main()
