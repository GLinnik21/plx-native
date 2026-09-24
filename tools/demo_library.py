#!/usr/bin/env python3
"""The demo library behind the documentation screenshots: fetch its openly licensed artwork, derive
the images the mock server serves, and write the credits.

    python3 tools/demo_library.py fetch     # download every pinned asset (sha256-checked)
    python3 tools/demo_library.py derive    # fetch, then build posters/backdrops/stills
    python3 tools/demo_library.py credits   # rewrite docs/screenshots/CREDITS.md and site/credits.html
    python3 tools/demo_library.py site-credits  # rewrite site/credits.html only (make screenshots does)
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
encoder settings, so one ffmpeg build derives byte-identical files every time. The clear logos
(`logo` in the catalog, see `derive_logo`) are cut from each film's own poster with Pillow, the
one Python package the screenshots need. An episode marked `stand_in` also gets a STAND-IN video
(`derive_stand_in`): black and silent, the episode's catalog length, so the simulator can play the
episode (the Up Next figure) without a copy of the work being fetched or shown.
"""
import argparse
import hashlib
import html
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
ASSETS = ROOT / "tests" / "demo_library" / "assets.json"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
CREDITS = ROOT / "docs" / "screenshots" / "CREDITS.md"
SITE_CREDITS = ROOT / "site" / "credits.html"
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
                pass
            try:  # the partial download never survives: a failed request, a bad hash, a ^C
                with open(tmp.name, "wb") as f, urllib.request.urlopen(req, timeout=120) as r:
                    shutil.copyfileobj(r, f)
                got = sha256(tmp.name)
                if got != a["sha256"]:
                    sys.exit(f"demo_library: {aid}: sha256 {got} != pinned {a['sha256']} ({a['url']})")
                os.replace(tmp.name, dst)
            finally:
                if os.path.exists(tmp.name):
                    os.unlink(tmp.name)
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


# The stand-in's encoding. Part of its recipe stamp, so a change here re-derives it.
STAND_IN = {"size": "640x360", "rate": 24, "audio_rate": 48000, "v": "libx264", "a": "aac"}


def derive_stand_in(dst, seconds):
    """A black, silent H.264/AAC MP4 of exactly `seconds`: what an episode marked `stand_in`
    plays in the simulator. It shows nothing of the work (the player figure that uses it draws no
    video plane), so it needs no source and no credit."""
    c = STAND_IN
    _ffmpeg(["-f", "lavfi", "-i", f"color=c=black:s={c['size']}:r={c['rate']}:d={seconds}",
             "-f", "lavfi", "-i", f"anullsrc=r={c['audio_rate']}:cl=stereo",
             "-t", str(seconds), "-c:v", c["v"], "-preset", "veryfast", "-tune", "stillimage",
             "-pix_fmt", "yuv420p", "-c:a", c["a"], "-b:a", "64k", "-movflags", "+faststart",
             "-map_metadata", "-1", "-bitexact", "-fflags", "+bitexact", str(dst)])


def _rgba(hexcolour):
    """`#rrggbb` → an opaque RGBA tuple."""
    v = hexcolour.lstrip("#")
    return tuple(int(v[i:i + 2], 16) for i in (0, 2, 4)) + (255,)


# What makes a pixel lettering rather than background, per key. Each is a 0..255 image the
# key's `levels` then stretch into alpha.
_SIGNALS = {
    "light": lambda r, g, b, lum: lum,
    "dark": lambda r, g, b, lum: lum.point(lambda v: 255 - v),
    "red": lambda r, g, b, lum: _dominance(r, g, b),
    "green": lambda r, g, b, lum: _dominance(g, r, b),
    "shade": lambda r, g, b, lum: _shade(lum),
}


def _shade(lum):
    """How much darker each pixel is than its local ground: the ground is the brightest nearby
    (a max filter over a quarter-size copy, wider than a letter stroke), softened."""
    from PIL import Image, ImageChops, ImageFilter
    small = lum.resize((max(1, lum.width // 4), max(1, lum.height // 4)))
    ground = small.filter(ImageFilter.MaxFilter(15)).filter(ImageFilter.GaussianBlur(15))
    return ImageChops.subtract(ground.resize(lum.size, Image.BILINEAR), lum).filter(ImageFilter.GaussianBlur(1.5))


def _dominance(a, b, c):
    """How far channel `a` stands above the larger of the other two, clamped at 0."""
    from PIL import ImageChops
    return ImageChops.subtract(a, ImageChops.lighter(b, c))


def derive_logo(paths, dst, recipe):
    """An item's clearLogo: the film's own title art, cut out of its poster onto a transparent
    ground and trimmed to its ink.

    `box` ([x, y, w, h] in poster pixels) is cut out and resized `scale`×. Each of `keys` —
    `{"signal": light|dark|shade|red|green, "levels": [lo, hi]}` — says what one part of the
    lettering looks like against that poster's ground (light letters on a dark sky; red letters),
    and is stretched from `lo` (transparent) to `hi` (opaque). A key may also carry:

    * `blur` — a Gaussian radius applied to the signal first, so a textured letter keys whole;
    * `grow: {"signal", "levels", "steps"}` — grow the key `steps` pixels into the region that
      second signal marks. Spring's carved-stone letters sit on mist that darkens down the
      poster, so no one `dark` level both holds their lit rims and drops the mist: the letter
      cores seed the key and grow out to the rims `shade` (darkness against the local ground)
      finds, never reaching the mist a letter does not touch;
    * `smooth` — the median size that cleans the edge (3), and `feather`, a final Gaussian radius;
    * `fill` (`#rrggbb`) — recolour that part's ink, for lettering too dark to read over a
      backdrop; otherwise the colours are the poster's own.

    The keys are layered in order. Pillow does the keying (the one Python package the screenshots
    need).
    """
    try:
        from PIL import Image, ImageChops, ImageFilter
    except ImportError:
        sys.exit("demo_library: the clear logos need Pillow: python3 -m pip install Pillow")
    x, y, w, h = recipe["box"]
    n = recipe.get("scale", 1)
    rgb = Image.open(paths[recipe["asset"]]).convert("RGB").crop((x, y, x + w, y + h))
    rgb = rgb.resize((round(w * n), round(h * n)), Image.LANCZOS)
    r, g, b = rgb.split()
    lum = rgb.convert("L")

    def keyed(k):
        sig = _SIGNALS[k["signal"]](r, g, b, lum)
        if k.get("blur"):
            sig = sig.filter(ImageFilter.GaussianBlur(k["blur"]))
        lo, hi = k["levels"]
        return sig.point(lambda v: 0 if v <= lo else 255 if v >= hi else (v - lo) * 255 // (hi - lo))

    logo = Image.new("RGBA", rgb.size, (0, 0, 0, 0))
    for key in recipe["keys"]:
        alpha = keyed(key)
        if "grow" in key:
            region = keyed(key["grow"])
            alpha = ImageChops.darker(alpha, region)
            for _ in range(key["grow"]["steps"]):
                alpha = ImageChops.darker(alpha.filter(ImageFilter.MaxFilter(3)), region)
        alpha = alpha.filter(ImageFilter.MedianFilter(key.get("smooth", 3)))
        if key.get("feather"):
            alpha = alpha.filter(ImageFilter.GaussianBlur(key["feather"]))
        ink = Image.new("RGBA", rgb.size, _rgba(key["fill"])) if "fill" in key else rgb.convert("RGBA")
        ink.putalpha(alpha)
        logo.alpha_composite(ink)
    logo = logo.crop(logo.getchannel("A").getbbox())
    logo.save(dst, format="PNG", optimize=False)


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
        if "logo" in rec:
            recipe = rec["logo"]
            dst = out / key.replace("/", "_") / "logo.png"
            dst.parent.mkdir(parents=True, exist_ok=True)
            stamp = dst.with_suffix(".recipe")
            want = json.dumps([assets[recipe["asset"]]["sha256"], recipe], sort_keys=True)
            if not dst.exists() or not stamp.exists() or stamp.read_text() != want:
                derive_logo(paths, dst, recipe)
                stamp.write_text(want)
            report[f"{key}:logo"] = sha256(dst)
        if rec.get("stand_in"):
            dst = out / key.replace("/", "_") / "stand-in.mp4"
            dst.parent.mkdir(parents=True, exist_ok=True)
            stamp = dst.with_suffix(".recipe")
            want = json.dumps([rec["minutes"], STAND_IN], sort_keys=True)
            if not dst.exists() or not stamp.exists() or stamp.read_text() != want:
                derive_stand_in(dst, rec["minutes"] * 60)
                stamp.write_text(want)
            report[f"{key}:stand-in"] = sha256(dst)
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
        if "stand_in" in rec:
            assert kind == "episode" and rec["stand_in"] is True and "media" not in rec, \
                f"{key}: stand_in is `true` on an episode without media"
        if "logo" in rec:
            # A clearLogo is the film's own title art, so it is cut from that film's own poster.
            assert rec["logo"]["asset"] == rec.get("poster", {}).get("asset"), \
                f"{key}: a logo is cut from the item's own poster"
            assert rec["logo"]["keys"], f"{key}: a logo needs at least one key"
            for k in rec["logo"]["keys"]:
                for sig in (k["signal"], k.get("grow", {}).get("signal", k["signal"])):
                    assert sig in _SIGNALS, f"{key}: logo key {sig!r}"
                if "fill" in k:
                    _rgba(k["fill"])
    for aid, a in assets.items():
        for field in ("url", "sha256", "bytes", "licence", "author", "attribution", "source_page"):
            assert a.get(field) not in (None, ""), f"asset {aid}: missing {field}"
        assert a["licence"].startswith(("CC BY", "CC0", "Public domain")), f"asset {aid}: licence {a['licence']!r}"
        assert "-SA" not in a["licence"] and "-NC" not in a["licence"] and "-ND" not in a["licence"], aid
    unused = sorted(set(assets) - used)
    assert not unused, f"assets never used: {unused}"
    for ref in [c["item"] for c in catalog["continue_watching"]] + catalog["watched"] + catalog["added_order"] \
            + [catalog["hero"]] + catalog.get("hero_alternatives", []):
        assert ref in seen, f"catalog refers to unknown item {ref!r}"
    for name, order in catalog.get("collection_order", {}).items():
        members = [m["id"] for m in catalog["movies"] if name in m.get("collections", [])]
        assert name in catalog["collections"], f"collection_order: no collection {name!r}"
        assert sorted(order) == sorted(members), \
            f"collection_order[{name!r}] must list every member exactly once: {sorted(members)}"
    return True


# ------------------------------------------------------------------ credits -------------------

def credits(assets, catalog, dst=CREDITS):
    where = {}
    titles = {}
    for key, kind, rec in item_keys(catalog):
        titles[key] = rec["title"]
        for role in ("poster", "art", "thumb", "media"):
            aid = rec.get(role, {}).get("asset") if role != "media" else rec.get("media")
            if aid:
                where.setdefault(aid, []).append((key, role))
        if "logo" in rec:
            recoloured = any("fill" in k for k in rec["logo"]["keys"])
            where.setdefault(rec["logo"]["asset"], []).append((key, "logo-fill" if recoloured else "logo"))
    label = {"poster": "poster", "art": "backdrop", "thumb": "episode still", "media": "video",
             "logo": "clear logo, derived from this poster",
             "logo-fill": "clear logo, derived from this poster, lettering recoloured"}
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
        "resized and cropped from the sources below; no other change was made to them. A film's clear",
        "logo (the title art over the home hero) is derived from that film's own poster, under the",
        "poster's licence: its title lettering is cut out onto a transparent ground and nothing is",
        "redrawn; where the table says so, lettering too dark to read over a backdrop is recoloured.",
        "",
        "Written by `make screenshots` with the images; do not edit by hand.",
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
    texts = {"CC BY 3.0": "https://creativecommons.org/licenses/by/3.0/",
             "CC BY 4.0": "https://creativecommons.org/licenses/by/4.0/",
             "CC0 1.0": "https://creativecommons.org/publicdomain/zero/1.0/"}
    used = sorted({a["licence"] for a in assets.values()} & set(texts))
    lines += ["", "Licence texts: " + ", ".join(f"[{name}]({texts[name]})" for name in used)
              + ". Public-domain status is as recorded on each file's Wikimedia Commons page.", ""]
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text("\n".join(lines))
    print(f"demo_library: wrote {dst}")


LICENCE_TEXTS = {"CC BY 3.0": "https://creativecommons.org/licenses/by/3.0/",
                 "CC BY 4.0": "https://creativecommons.org/licenses/by/4.0/",
                 "CC0 1.0": "https://creativecommons.org/publicdomain/zero/1.0/"}

# Where a source page lives, as the credits page names it.
SOURCE_HOSTS = {"commons.wikimedia.org": "Wikimedia Commons", "studio.blender.org": "Blender Studio",
                "esahubble.org": "ESA/Hubble", "archive.org": "Internet Archive"}


def _changes(role, recipe):
    """What was done to a source to make the image the app shows, in the credits page's words."""
    if role == "media":
        return "Played in the app as the film itself; not altered."
    if role == "logo":
        text = "Title lettering cut out of the poster onto a transparent ground for the clear logo"
        return text + ("; dark lettering recoloured to read over a backdrop." if any("fill" in k for k in recipe["keys"])
                       else ".")
    w, h = {"poster": POSTER, "art": ART, "thumb": THUMB}[role]
    what = {"poster": "poster", "art": "backdrop", "thumb": "episode still"}[role]
    if role == "art" and recipe.get("mode") == "extend":
        return (f"Scaled to fit a {w}×{h} {what}; the sides are filled with a blurred, "
                "darkened copy of the image.")
    return f"Scaled and cropped to a {w}×{h} {what}."


def _source_host(url):
    host = urllib.parse.urlsplit(url).hostname
    return SOURCE_HOSTS.get(host, host)


def site_works(assets, catalog):
    """The credits page's model: one entry per work (a film or a series, in catalog order), each a
    list of credited sources. A source used several ways within a work is one entry; the episode
    stills of a series that share author, licence and treatment are one entry with a link per
    episode."""
    works = []
    for kind, records in (("film", catalog["movies"]), ("series", catalog["shows"])):
        for rec in records:
            uses = {}  # asset id -> {"roles": [...], "changes": [...], "episodes": [(sort, label)]}

            def use(aid, role, recipe, episode=None):
                u = uses.setdefault(aid, {"roles": [], "changes": [], "episodes": []})
                label = {"poster": "Poster", "art": "Backdrop", "thumb": "Episode still", "media": "Video",
                         "logo": "Clear logo"}[role]
                if label not in u["roles"]:
                    u["roles"].append(label)
                change = _changes(role, recipe)
                if change not in u["changes"]:
                    u["changes"].append(change)
                if episode:
                    u["episodes"].append(episode)

            for role in ("poster", "art", "thumb", "logo"):
                if role in rec:
                    use(rec[role]["asset"], role, rec[role])
            if "media" in rec:
                use(rec["media"], "media", None)
            for season in rec.get("seasons", []):
                for e in season["episodes"]:
                    if "thumb" in e:
                        use(e["thumb"]["asset"], "thumb", e["thumb"],
                            ((season["index"], e["index"]), f"S{season['index']} E{e['index']}: {e['title']}"))
            entries, stills = [], {}
            for aid, u in uses.items():
                a = assets[aid]
                who = a["author"] if a["attribution"] in ("", a["author"]) else f"{a['author']}; {a['attribution']}"
                if u["roles"] == ["Episode still"]:
                    key = (who, a["licence"], tuple(u["changes"]))
                    if key not in stills:
                        stills[key] = {"roles": u["roles"], "author": who, "licence": a["licence"],
                                       "changes": u["changes"], "sources": []}
                        entries.append(stills[key])
                    stills[key]["sources"] += [(sort, label, a["source_page"]) for sort, label in u["episodes"]]
                    continue
                entries.append({"roles": u["roles"], "author": who, "licence": a["licence"], "changes": u["changes"],
                                "sources": [((), _source_host(a["source_page"]), a["source_page"])]})
            for e in entries:
                e["sources"].sort()
                if len(e["sources"]) > 1:
                    e["roles"] = ["Episode stills"]
            works.append({"id": rec["id"], "kind": kind, "title": rec["title"], "year": rec.get("year"),
                          "entries": entries})
    return works


def site_credits(assets, catalog, dst=SITE_CREDITS):
    """site/credits.html: the attribution the website's screenshots owe, written from the same
    manifests as CREDITS.md so it cannot drift from them. It lists the whole demo library: which
    works a figure shows is decided by the app's layout at capture time, not by anything the
    manifests record."""
    esc = html.escape
    works = site_works(assets, catalog)

    def licence(name):
        if name in LICENCE_TEXTS:
            return f'<a href="{esc(LICENCE_TEXTS[name])}" rel="license">{esc(name)}</a>'
        return esc(name)

    def work(w):
        year = f' <span class="credit-year">{w["year"]}</span>' if w["year"] else ""
        out = [f'          <article class="credit-work" id="{esc(w["id"])}">',
               f'            <h3 class="credit-title">{esc(w["title"])}{year}</h3>',
               '            <div class="credit-entries">']
        for e in w["entries"]:
            if len(e["sources"]) == 1:
                (_, text, href), = e["sources"]
                source = f'<a href="{esc(href)}">{esc(text)}</a>'
            else:  # a link per episode, each named for its episode
                source = ('<ul class="credit-sources">'
                          + "".join(f'<li><a href="{esc(href)}">{esc(text)}</a></li>' for _, text, href in e["sources"])
                          + "</ul>")
            roles = e["roles"][0] + "".join(f" and {r.lower()}" for r in e["roles"][1:])
            facts = [("By", esc(e["author"])), ("Licence", licence(e["licence"])), ("Source", source),
                     ("Changes", esc(" ".join(e["changes"])))]
            out += ['              <dl class="credit-entry">',
                    f'                <dt class="credit-use">{esc(roles)}</dt>',
                    *(f'                <dd><span class="credit-label">{label}</span>'
                      f'<div class="credit-value">{value}</div></dd>' for label, value in facts),
                    '              </dl>']
        out += ['            </div>', '          </article>']
        return out

    def section(kind, heading):
        body = [line for w in works if w["kind"] == kind for line in work(w)]
        return [f'        <section class="credits-section" aria-labelledby="credits-{kind}">',
                f'          <h2 id="credits-{kind}">{heading}</h2>', *body, '        </section>', '']

    used = [name for name in LICENCE_TEXTS if any(a["licence"] == name for a in assets.values())]
    licences = ", ".join(f'<a href="{esc(LICENCE_TEXTS[n])}" rel="license">{esc(n)}</a>' for n in used)
    repo = "https://github.com/GLinnik21/plx-native/blob/main/"
    sections = "\n".join(section("film", "Films") + section("series", "Series"))
    page = f"""<!doctype html>
<!-- Written by `python3 tools/demo_library.py site-credits` (make screenshots) from
     tests/demo_library/assets.json and catalog.json. Do not edit by hand. -->
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="description" content="Credits and licences for the openly licensed artwork in the PlxNative screenshots." />
    <title>Artwork credits — PlxNative</title>
    <link rel="canonical" href="https://plxnative.com/credits.html" />
    <link rel="icon" type="image/png" href="assets/logo-master.png" />
    <meta name="theme-color" content="#202022" />
    <link rel="stylesheet" href="styles.css" />
  </head>
  <body>
    <div class="page-shell">
      <div class="ambient-ground" aria-hidden="true"></div>
      <main class="page-root">
        <header class="site-header">
          <a class="brand" href="./" aria-label="PlxNative home">
            <span class="brand-mark"><img src="assets/logo-master.png" alt="" /></span>
            <span class="brand-name">PlxNative</span>
          </a>
          <div class="header-actions">
            <a class="header-back" href="./">&larr; Back to the site</a>
          </div>
        </header>

        <section class="credits-intro" aria-labelledby="credits-title">
          <h1 id="credits-title">Artwork credits</h1>
          <p class="lead">
            The screenshots on this site show PlxNative browsing a demo library made entirely of openly
            licensed and public-domain works: open movies by Blender Studio and others.
          </p>
          <p>
            Each work is listed with its author, licence, source and the changes made. Every picture was
            scaled and cropped to the app&rsquo;s layout, and the screenshots themselves are cropped and
            resized for this site. Nothing was redrawn or otherwise altered except where noted.
          </p>
        </section>

{sections}
        <footer class="site-footer credits-footer">
          <p class="credits-licences">
            Licence texts: {licences}. Public-domain status is as recorded on each work&rsquo;s source page.
          </p>
          <p class="footer-meta">
            <span>Generated from the <a href="{repo}tests/demo_library/assets.json">demo library manifest</a></span>
            <span class="sep" aria-hidden="true">·</span>
            <span><a href="{repo}docs/screenshots/CREDITS.md">Documentation screenshot credits</a></span>
          </p>
        </footer>
      </main>
    </div>
  </body>
</html>
"""
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(page)
    print(f"demo_library: wrote {dst}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("command", choices=["fetch", "derive", "credits", "site-credits", "check"])
    a = ap.parse_args()
    assets, catalog = load()
    check(assets, catalog)
    if a.command == "fetch":
        fetch(assets)
    elif a.command == "derive":
        derive(assets, catalog)
    elif a.command == "credits":
        credits(assets, catalog)
        site_credits(assets, catalog)
    elif a.command == "site-credits":
        site_credits(assets, catalog)
    else:
        print("demo_library: manifests ok")


if __name__ == "__main__":
    main()
