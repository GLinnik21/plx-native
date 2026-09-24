#!/usr/bin/env python3
"""The demo library behind the documentation screenshots: fetch its openly licensed artwork, derive
the images the mock server serves, and write the credits.

    python3 tools/demo_library.py fetch     # download every pinned asset (sha256-checked)
    python3 tools/demo_library.py derive    # fetch, then build posters/backdrops/stills
    python3 tools/demo_library.py credits   # rewrite docs/screenshots/CREDITS.md
    python3 tools/demo_library.py check     # validate the two manifests; no network

Two committed manifests drive it:

* `tests/demo_library/assets.json` — every SOURCE file: pinned URL, sha256, byte size, licence,
  author, attribution. A download whose hash differs is refused, so the artwork cannot drift under
  the screenshots without the manifest changing in review.
* `tests/demo_library/catalog.json` — the library itself (titles, credits, synopses, watch state,
  shelves) and, per item, which asset each image is derived from and how (`mode`, `anchor`, `crop`).

Nothing is committed but the manifests: the sources and the derived images live in a cache outside
the repository (`$PLXNATIVE_DEMO_CACHE`, default `~/.cache/plxnative-demo`, ~390 MB of sources,
283 MB of it the complete Sintel the player figure plays), so a fresh clone rebuilds them with one
command. Derivation is ffmpeg with fixed filters and fixed
encoder settings, so one ffmpeg build derives byte-identical files every time.
"""
import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
ASSETS = ROOT / "tests" / "demo_library" / "assets.json"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
CREDITS = ROOT / "docs" / "screenshots" / "CREDITS.md"
UA = "PlxNativeDemoLibrary/1.0 (+https://github.com/GLinnik21/plex-native-poc)"

POSTER = (600, 900)
ART = (1920, 1080)
THUMB = (1280, 720)


def cache_dir():
    env = os.environ.get("PLXNATIVE_DEMO_CACHE")
    return pathlib.Path(env) if env else pathlib.Path.home() / ".cache" / "plxnative-demo"


def load():
    return json.loads(ASSETS.read_text())["assets"], json.loads(CATALOG.read_text())


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def source_path(aid, asset):
    ext = asset["url"].rsplit(".", 1)[-1].lower()
    return cache_dir() / "src" / f"{aid}.{ext}"


def fetch(assets, only=None):
    """Download what is missing; verify everything. Returns {asset id: path}."""
    out = {}
    for aid, a in sorted(assets.items()):
        if only is not None and aid not in only:
            continue
        dst = source_path(aid, a)
        if not dst.exists() or sha256(dst) != a["sha256"]:
            dst.parent.mkdir(parents=True, exist_ok=True)
            print(f"demo_library: fetch {aid} ({a['bytes']:,} bytes)", flush=True)
            req = urllib.request.Request(a["url"], headers={"User-Agent": UA})
            with tempfile.NamedTemporaryFile(dir=dst.parent, delete=False) as tmp:
                with urllib.request.urlopen(req, timeout=120) as r:
                    shutil.copyfileobj(r, tmp)
            got = sha256(tmp.name)
            if got != a["sha256"]:
                os.unlink(tmp.name)
                sys.exit(f"demo_library: {aid}: sha256 {got} != pinned {a['sha256']} ({a['url']})")
            os.replace(tmp.name, dst)
        out[aid] = dst
    return out


# ------------------------------------------------------------------ derivation ----------------

def _ffmpeg(args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-threads", "1"] + args,
                   check=True)


def _cover(w, h, anchor):
    """Scale to COVER w×h, then crop — at the centre, an edge, or `upper` (30% down: the face line
    of a portrait still cut to 16:9)."""
    x = {"right": "iw-ow", "left": "0"}.get(anchor, "(iw-ow)/2")
    y = "(ih-oh)*3/10" if anchor == "upper" else "(ih-oh)/2"  # upper: a portrait still's faces
    return (f"scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,"
            f"crop={w}:{h}:{x}:{y}")


def derive_image(src, dst, size, recipe):
    """One image, per its recipe:

    * `mode: cover` (default) — fill the frame and crop at `anchor` (`center`/`right`/`left`/`upper`).
    * `mode: extend` — for wide key art: fit the whole picture to the frame's HEIGHT, pin it to
      `anchor`, and fill the rest with a blurred, darkened stretch of itself. This is how a 2.9:1
      banner becomes a 16:9 backdrop without cropping its subject out of the frame.
    * `crop: focus-right` / `upper` — a named anchor for a cover crop (poster from a landscape still).
    """
    w, h = size
    mode = recipe.get("mode", "cover")
    anchor = recipe.get("anchor") or {"focus-right": "right", "upper": "upper"}.get(recipe.get("crop", ""), "center")
    if mode == "extend":
        x = "W-w" if anchor == "right" else "(W-w)/2"
        graph = (f"[0:v]format=rgb24,split[a][b];"
                 f"[a]{_cover(w, h, 'center')},boxblur=40:3,eq=brightness=-0.10[bg];"
                 f"[b]scale=-2:{h}:flags=lanczos,crop='min(iw,{w})':{h}:'max(iw-{w},0)':0[fg];"
                 f"[bg][fg]overlay=x={x}:y=0,format=yuvj444p[o]")
    else:
        graph = f"[0:v]format=rgb24,{_cover(w, h, anchor)},format=yuvj444p[o]"
    _ffmpeg(["-i", str(src), "-filter_complex", graph, "-map", "[o]", "-frames:v", "1",
             "-q:v", "3", "-bitexact", str(dst)])


def item_keys(catalog):
    """(key, kind, record) for every item: `slug` for movies/shows, `slug/season/episode`."""
    for m in catalog["movies"]:
        yield m["id"], "movie", m
    for s in catalog["shows"]:
        yield s["id"], "show", s
        for season in s["seasons"]:
            for e in season["episodes"]:
                yield f"{s['id']}/{season['index']}/{e['index']}", "episode", e


def derived_dir():
    return cache_dir() / "derived"


def derive(assets, catalog):
    paths = fetch(assets)
    out = derived_dir()
    out.mkdir(parents=True, exist_ok=True)
    report = {}
    for key, kind, rec in item_keys(catalog):
        jobs = []
        if "poster" in rec:
            jobs.append(("poster", POSTER, rec["poster"]))
        if "art" in rec:
            jobs.append(("art", ART, rec["art"]))
        if "thumb" in rec:
            jobs.append(("thumb", THUMB, rec["thumb"]))
        for role, size, recipe in jobs:
            dst = out / key.replace("/", "_") / f"{role}.jpg"
            dst.parent.mkdir(parents=True, exist_ok=True)
            stamp = dst.with_suffix(".recipe")
            want = json.dumps([assets[recipe["asset"]]["sha256"], size, recipe], sort_keys=True)
            if not dst.exists() or not stamp.exists() or stamp.read_text() != want:
                derive_image(paths[recipe["asset"]], dst, size, recipe)
                stamp.write_text(want)
            report[f"{key}:{role}"] = sha256(dst)
    for m in catalog["movies"]:
        if "media" in m:
            report[f"{m['id']}:media"] = assets[m["media"]]["sha256"]
    (out / "derived.json").write_text(json.dumps(report, indent=1, sort_keys=True))
    print(f"demo_library: {len(report)} derived files in {out}", flush=True)
    return out


def media_path(assets, aid):
    return source_path(aid, assets[aid])


# ------------------------------------------------------------------ validation ----------------

def check(assets, catalog):
    """Every referenced asset exists and carries a licence; every asset is referenced; ids unique."""
    used = set()
    seen = set()
    for key, kind, rec in item_keys(catalog):
        assert key not in seen, f"duplicate item {key}"
        seen.add(key)
        for role in ("poster", "art", "thumb"):
            if role in rec:
                aid = rec[role]["asset"]
                assert aid in assets, f"{key}: {role} asset {aid!r} is not in assets.json"
                used.add(aid)
        if "media" in rec:
            assert rec["media"] in assets, f"{key}: media {rec['media']!r} is not in assets.json"
            used.add(rec["media"])
    for aid, a in assets.items():
        for field in ("url", "sha256", "bytes", "licence", "author", "attribution", "source_page"):
            assert a.get(field) not in (None, ""), f"asset {aid}: missing {field}"
        assert a["licence"].startswith(("CC BY", "Public domain")), f"asset {aid}: licence {a['licence']!r}"
        assert "-SA" not in a["licence"] and "-NC" not in a["licence"] and "-ND" not in a["licence"], aid
    unused = sorted(set(assets) - used)
    assert not unused, f"assets never used: {unused}"
    for ref in [c["item"] for c in catalog["continue_watching"]] + catalog["watched"] + catalog["added_order"] \
            + [catalog["hero"]] + catalog.get("hero_alternatives", []):
        assert ref in seen, f"catalog refers to unknown item {ref!r}"
    return True


# ------------------------------------------------------------------ credits -------------------

def credits(assets, catalog):
    where = {}
    titles = {}
    for key, kind, rec in item_keys(catalog):
        titles[key] = rec["title"]
        for role in ("poster", "art", "thumb", "media"):
            aid = rec.get(role, {}).get("asset") if role != "media" else rec.get("media")
            if aid:
                where.setdefault(aid, []).append((key, role))
    label = {"poster": "poster", "art": "backdrop", "thumb": "episode still", "media": "video"}
    show_of = {}
    for s in catalog["shows"]:
        for season in s["seasons"]:
            for e in season["episodes"]:
                show_of[f"{s['id']}/{season['index']}/{e['index']}"] = s["title"]
    lines = [
        "# Screenshot credits",
        "",
        "Every picture inside the documentation screenshots comes from an openly licensed or public-domain",
        "work. The library is `tests/demo_library/catalog.json`; the files, their pinned hashes and licences",
        "are `tests/demo_library/assets.json`; `make screenshots` rebuilds the images from them",
        "(`.agents/skills/ui-sim/SKILL.md`, \"Documentation screenshots\"). Backdrops and posters are",
        "resized and cropped from the sources below; no other change was made to them. Titles are drawn",
        "by the app as text: film title logos are not used, because the Blender Studio terms that license",
        "its artwork reserve all rights in trademarks and logos.",
        "",
        "Generated by `python3 tools/demo_library.py credits`; do not edit by hand.",
        "",
        "| Used as | Source | Licence | Author / attribution |",
        "| --- | --- | --- | --- |",
    ]
    for aid, a in sorted(assets.items(), key=lambda kv: (where.get(kv[0], [("~", "")])[0][0], kv[0])):
        uses = []
        for key, role in where.get(aid, []):
            name = titles[key] if key not in show_of else f"{show_of[key]}: {titles[key]}"
            uses.append(f"{name} ({label[role]})")
        attribution = a["author"] if a["attribution"] in ("", a["author"]) else f"{a['author']}; {a['attribution']}"
        lines.append(f"| {', '.join(uses)} | [{a['url'].rsplit('/', 1)[-1]}]({a['source_page']}) | "
                     f"{a['licence']} | {attribution.replace('|', '/')} |")
    lines += ["", "Licence texts: [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/), "
                  "[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Public-domain status is as "
                  "recorded on each file's Wikimedia Commons page.", ""]
    CREDITS.parent.mkdir(parents=True, exist_ok=True)
    CREDITS.write_text("\n".join(lines))
    print(f"demo_library: wrote {CREDITS.relative_to(ROOT)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("command", choices=["fetch", "derive", "credits", "check"])
    a = ap.parse_args()
    assets, catalog = load()
    check(assets, catalog)
    if a.command == "fetch":
        fetch(assets)
    elif a.command == "derive":
        derive(assets, catalog)
    elif a.command == "credits":
        credits(assets, catalog)
    else:
        print("demo_library: manifests ok")


if __name__ == "__main__":
    main()
