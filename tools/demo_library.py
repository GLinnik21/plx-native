#!/usr/bin/env python3
"""The demo library behind the documentation screenshots: fetch its openly licensed artwork, derive
the images the mock server serves, and write the credits.

    python3 tools/demo_library.py fetch     # download every pinned asset (sha256 and pixel size checked)
    python3 tools/demo_library.py derive    # fetch, then build posters/backdrops/stills
    python3 tools/demo_library.py credits   # rewrite docs/screenshots/CREDITS.md and site/credits.html
    python3 tools/demo_library.py site-credits  # rewrite site/credits.html only (make screenshots does)
    python3 tools/demo_library.py check     # validate the two manifests; no network
    python3 tools/demo_library.py check --complete  # also: every title and episode complete, bar the
                                            # shrink-only list in tests/demo_library/pending.json
    python3 tools/demo_library.py hero-report  # per hero-eligible title: where its declared
                                            # `art.subject` box lands in the 1920x1080 frame, the
                                            # zones it overlaps and the verdict; no network, no images
    python3 tools/demo_library.py fixtures  # capture the two fixture titles' detail responses

Two committed manifests drive it:

* `tests/demo_library/assets.json` — every SOURCE file: pinned URL, sha256, byte size, kind,
  pixel width and height, licence, author, attribution. A download whose hash differs is refused, so the artwork cannot drift under
  the screenshots without the manifest changing in review.
* `tests/demo_library/catalog.json` — the library itself (titles, credits, synopses, watch state,
  shelves) and, per item, which asset each image is derived from and how (`mode`, `anchor`, `crop`).

Hero art. A title the home hero can show (the pinned hero and alternatives, Continue Watching,
Recently Added) must have its subject on the RIGHT with logo and text off the centre. Its catalog
`art` declares `subject: [x0, y0, x1, y1]` in SOURCE pixels, read off the image by a person;
`check --complete` maps that box into the frame by arithmetic from the recipe (`mode: cover` only,
`anchor`/`crop`) and the asset's recorded width and height, and fails the title (category
`hero_art`) when the box is missing, outside the source, cut off by the crop, left of 60% of the
frame, or over a text zone. The zones are copied in `HERO_PINS`, each pinned to the Rust constant
it comes from, and `check --complete` re-reads that source so a layout change cannot leave the
check stale. Titles that fail are listed in `pending.json` under `hero_art`; the list only shrinks,
and the demo-video workflow (stage S6 of the plan; not in the tree yet) is to refuse to run while
it, or any pending entry, is non-empty.

Per-title flags in `catalog.json`, all checked by `check`:

* `not_hero: true` — the title can never be the home hero: `hero_eligible` leaves it out, so it needs
  no `art.subject`, and `check` refuses it where the app's hero pool could take it from: the pinned
  hero, `hero_alternatives`, or one of the first `HERO_MAX` slots of the pool `hero_pool` rebuilds
  from the catalog the way `data/src/pms.rs` merges it (a test holds that to what the mock's Home
  answers). The mock refuses `--hero` on it too. Set it on a title whose backdrop does not look
  right behind the hero.
* `not_in_video: true` — the title is never opened or featured in the site video (the future
  storyboard gate reads it; nothing renders yet). A title in `logo_none_approved` (its hero and
  detail page show the text title, not a clear logo) must carry it; the pinned hero and
  `hero_alternatives` cannot (the video opens the hero).
* `poster.still_as_poster: true` — the poster is a film still, not a poster: allowed only for a title
  listed in the catalog's `still_poster_approved`, and then the asset may be of kind `still` or
  `backdrop`.

A clear logo is cut from the title's own poster (`logo-from-poster`: asset kind `poster`), or is the
film's own official title card or a frame of the film: asset kind `logo-title-card`, which records
`of` (the title id it belongs to) and `origin` (`official-title-card` or `film-frame`). It is a
source for that title's logo and for nothing else, and takes the same licences as every other asset.

Nothing is committed but the manifests and the two captured detail fixtures: the sources and the derived images live in a cache outside
the repository (`$PLXNATIVE_DEMO_CACHE`, default `~/.cache/plxnative-demo`, ~390 MB of sources,
283 MB of it the complete Sintel the player figure plays), so a fresh clone rebuilds them with one
command. Derivation is ffmpeg with fixed filters and fixed
encoder settings, so one ffmpeg build derives byte-identical files every time. The clear logos
(`logo` in the catalog, see `derive_logo`) are cut from each film's own poster by a keyed recipe,
or, for a `logo-title-card` asset, trimmed to the lettering of the film's own title card; both
are done with Pillow, the one Python package the screenshots need. An episode marked `stand_in` also gets a STAND-IN video
(`derive_stand_in`): black and silent, the episode's catalog length, so the simulator can play the
episode (the Up Next figure) without a copy of the work being fetched or shown.
"""
import argparse
import hashlib
import html
import json
import os
import pathlib
import re
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
        got = _dimensions(dst, a)
        if got != (a["width"], a["height"]):
            sys.exit(f"demo_library: {aid}: {got[0]}x{got[1]} != pinned {a['width']}x{a['height']} "
                     f"in assets.json ({dst})")
        out[aid] = dst
    return out


def _dimensions(path, asset):
    """(width, height) of a source: an image through Pillow (which reads the header, not the
    pixels), the one `media` video through ffprobe, so the manifest's sizes are checked against
    the files themselves and the hero-art geometry can trust them without opening anything."""
    if asset.get("kind") == "media":
        out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                              "stream=width,height", "-of", "csv=p=0", str(path)],
                             capture_output=True, text=True, check=True).stdout
        w, h = out.strip().split(",")
        return int(w), int(h)
    from PIL import Image
    with Image.open(path) as im:
        return im.size


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


def recipe_anchor(recipe):
    """Where a cover crop sits: the recipe's `anchor`, else the one its named `crop` stands for,
    else the centre. The deriver and the hero-art geometry (`hero_map_box`) both read it here."""
    return recipe.get("anchor") or {"focus-right": "right", "upper": "upper"}.get(recipe.get("crop", ""), "center")


def recipe_stamp(recipe):
    """The part of an image recipe that decides its pixels. `subject` (the hero-art box a person
    read off the source) is a declaration about the picture, not a step in making it, so declaring
    or correcting one never re-derives an image."""
    return {k: v for k, v in recipe.items() if k != "subject"}


def derive_image(src, dst, size, recipe):
    """One image, per its recipe:

    * `mode: cover` (default) — fill the frame and crop at `anchor` (`center`/`right`/`left`/`upper`).
    * `mode: extend` — for wide key art: fit the whole picture to the frame's HEIGHT, pin it to
      `anchor`, and fill the rest with a blurred, darkened stretch of itself. This is how a 2.9:1
      banner becomes a 16:9 backdrop without cropping its subject out of the frame. Art a hero can
      show may not use it (the pad boundary is a visible seam in the text zone): `hero_geometry`
      refuses `extend`, so such a title is `cover` with a declared `art.subject`.
    * `crop: focus-right` / `upper` — a named anchor for a cover crop (poster from a landscape still).
    """
    w, h = size
    mode = recipe.get("mode", "cover")
    anchor = recipe_anchor(recipe)
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
    if not recipe.get("keys"):
        # A film's own title card (asset kind `logo-title-card`) is lettering on a transparent ground
        # already: trim it to its ink and keep every pixel as it was.
        with Image.open(paths[recipe["asset"]]) as src:
            if src.mode not in ("RGBA", "LA") and "transparency" not in src.info:
                sys.exit(f"demo_library: {recipe['asset']} has no transparent ground; cut it with `keys`")
            card = src.convert("RGBA")
        card.crop(card.getchannel("A").getbbox()).save(dst, format="PNG", optimize=False)
        return
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
            want = json.dumps([assets[recipe["asset"]]["sha256"], size, recipe_stamp(recipe)], sort_keys=True)
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

# The review-score marks a catalog `ratings` row may name: the ones the app badges
# (`data/src/metadata.rs`, `RatingArt`) minus what this library never claims. The TMDB mark is
# refused (its terms forbid this use), Rotten Tomatoes' "certified" is a state only RT itself
# awards, and Metacritic has no badge in the app.
RATING_IMAGES = frozenset({
    "imdb://image.rating",
    "rottentomatoes://image.rating.ripe", "rottentomatoes://image.rating.rotten",
    "rottentomatoes://image.rating.upright", "rottentomatoes://image.rating.spilled",
})
DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def _cited(key, what, row):
    """A real value is shown only with where it came from and when it was read."""
    src = row.get("source")
    assert isinstance(src, str) and src.startswith("https://"), f"{key}: {what} needs an https `source`"
    assert DATE.match(str(row.get("retrieved", ""))), f"{key}: {what} needs a `retrieved` date (YYYY-MM-DD)"


def check_cited_metadata(key, kind, rec):
    """The optional metadata the mock serves beyond the basics. Nothing here may be invented: a
    content rating and every review score carry their source and retrieval date, a tagline is our
    own text and says so (`demo_values`), and the lists are well formed."""
    if "contentRating" in rec:
        row = rec["contentRating"]
        assert isinstance(row, dict) and row.get("value"), f"{key}: contentRating is {{value, source, retrieved}}"
        _cited(key, "contentRating", row)
    for row in rec.get("ratings", []):
        assert row.get("image") in RATING_IMAGES, \
            f"{key}: rating image {row.get('image')!r} is not one of {sorted(RATING_IMAGES)}"
        assert row.get("type") in ("critic", "audience"), f"{key}: rating type"
        assert isinstance(row.get("value"), (int, float)) and 0 < row["value"] <= 10, \
            f"{key}: a rating value is on PMS's 0-10 scale"
        _cited(key, "a rating", row)
    if "tagline" in rec:
        assert rec["tagline"] and "tagline" in rec.get("demo_values", []), \
            f"{key}: a tagline is our own text and is listed in `demo_values`"
    for field in ("countries", "creators"):
        if field in rec:
            assert isinstance(rec[field], list) and rec[field] and all(isinstance(n, str) and n for n in rec[field]), \
                f"{key}: {field} is a non-empty list of names"
    assert "creators" not in rec or kind == "show", f"{key}: only a show has creators"
    # An empty credit list is a claim, so it is cited: `cast_none` / `writers_none` say that the
    # source was read and credits no performer / writer. They stand in for the list, not beside it.
    for none, listed in (("cast_none", "cast"), ("writers_none", "writers")):
        if none in rec:
            _cited(key, none, rec[none])
            assert not rec.get(listed), f"{key}: {none} contradicts the {listed} it lists"
    for src in rec.get("sources", []):
        _cited(key, "a source", dict(src, source=src.get("url")))
        assert src.get("fields"), f"{key}: a source names the fields it supplied"


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
                assert assets[aid].get("kind") != "logo-title-card", \
                    f"{key}: {role} uses a title card, which is a source for a clear logo only"
                used.add(aid)
        check_cited_metadata(key, kind, rec)
        if "media" in rec:
            assert rec["media"] in assets, f"{key}: media {rec['media']!r} is not in assets.json"
            assert assets[rec["media"]].get("kind") == "media", f"{key}: media is an asset of kind media"
            used.add(rec["media"])
        if "stand_in" in rec:
            assert kind == "episode" and rec["stand_in"] is True and "media" not in rec, \
                f"{key}: stand_in is `true` on an episode without media"
        if "logo" in rec:
            # A clearLogo is the film's own title art: cut from that film's own poster, or the
            # film's own official title card / a frame of it (`logo-title-card`, which names its film).
            lid = rec["logo"]["asset"]
            assert lid in assets, f"{key}: logo asset {lid!r} is not in assets.json"
            used.add(lid)
            if assets[lid].get("kind") == "logo-title-card":
                assert assets[lid].get("of") == key, \
                    f"{key}: a title-card logo is the item's own card (asset {lid!r} is of {assets[lid].get('of')!r})"
            else:
                assert lid == rec.get("poster", {}).get("asset"), f"{key}: a logo is cut from the item's own poster"
                assert rec["logo"]["keys"], f"{key}: a logo needs at least one key"
            for k in rec["logo"].get("keys", []):
                for sig in (k["signal"], k.get("grow", {}).get("signal", k["signal"])):
                    assert sig in _SIGNALS, f"{key}: logo key {sig!r}"
                if "fill" in k:
                    _rgba(k["fill"])
    for aid, a in assets.items():
        for field in ("url", "sha256", "bytes", "licence", "author", "attribution", "source_page"):
            assert a.get(field) not in (None, ""), f"asset {aid}: missing {field}"
        assert a["licence"].startswith(("CC BY", "CC0", "Public domain")), f"asset {aid}: licence {a['licence']!r}"
        assert "-SA" not in a["licence"] and "-NC" not in a["licence"] and "-ND" not in a["licence"], aid
        assert a.get("kind") in ASSET_KINDS, f"asset {aid}: kind {a.get('kind')!r} is not one of {sorted(ASSET_KINDS)}"
        for field in ("width", "height"):
            assert type(a.get(field)) is int and a[field] > 0, f"asset {aid}: {field} is a positive pixel count"
        if a["kind"] == "logo-title-card":
            assert a.get("of") in seen, f"asset {aid}: a title card names the item it belongs to in `of`"
            assert a.get("origin") in TITLE_CARD_ORIGINS, \
                f"asset {aid}: a title card's `origin` is one of {sorted(TITLE_CARD_ORIGINS)}"
    check_flags(assets, catalog, seen)
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


# ------------------------------------------------------------------ hero art geometry --------

# The home hero and the detail page both paint a title's derived 1920x1080 backdrop full bleed
# with text laid over its left; the owner's rule is that the picture's SUBJECT is on the RIGHT and
# neither logo nor text covers the centre of the composition. A machine cannot see a subject, so a
# person reads it off the SOURCE image and declares it, `art.subject = [x0, y0, x1, y1]` in source
# pixels; this module then does the part a machine can: map that box into the frame by arithmetic
# (cover scale, then the crop at the anchor, from the asset's recorded width and height; no image
# is opened, so it runs offline on Linux and in `make check`) and require it to be clear of every
# zone the layout draws text or chrome in. Whether the box really covers the subject stays the
# owner's eye (the signed contact sheet).

# What a `cover` crop at each named anchor keeps: the share of the horizontal / vertical overflow
# cut off the left / top. `_cover`'s crop expressions as numbers; any other anchor is the centre.
COVER_ANCHORS = {"right": (1.0, 0.5), "left": (0.0, 0.5), "upper": (0.5, 0.3)}
# The subject's centre must be at or right of 60% of the frame (the owner's rule, not a layout fact).
HERO_CENTRE_MIN_X = 0.60 * ART[0]
# ffmpeg rounds the scaled picture to whole pixels, so the mapped box is good to about one.
HERO_FRAME_EPS = 1.0

# The layout numbers the zones come from, each COPIED here and pinned to where it lives in the Rust
# source: `check_hero_pins` re-reads the source and fails when a copy has drifted, so a layout change
# cannot leave the check judging a frame that no longer exists. name -> [file, kind, key, value]:
#   const    `const KEY: T = <expression>;` (the expression may name other pins), value the number
#   literal  the number captured by the regex KEY (a figure only a test or a comment states)
#   text     the source text KEY must still contain (a formula the zones rebuild), value None
# SCR_W / SCR_H come first: later expressions are written in them.
HERO_PINS = {
    "SCR_W": ["rust-modules/base/src/surface.rs", "const", "LOGICAL_W", 1920.0],
    "SCR_H": ["rust-modules/base/src/surface.rs", "const", "LOGICAL_H", 1080.0],
    "MARGIN_X": ["rust-modules/ui/src/consts.rs", "const", "MARGIN_X", 96.0],
    "PEEK_Y": ["rust-modules/ui/src/consts.rs", "const", "PEEK_Y", 811.0],
    "TITLE_DY": ["rust-modules/ui/src/consts.rs", "const", "TITLE_DY", 34.0],
    "COL_W": ["rust-modules/ui/src/landing_hero.rs", "const", "COL_W", 660.0],
    "TEXT_BOTTOM": ["rust-modules/ui/src/landing_hero.rs", "const", "TEXT_BOTTOM", 692.0],
    "SPACE_MD": ["rust-modules/ui/src/theme.rs", "const", "space::MD", 24.0],
    "CTRL_H": ["rust-modules/ui/src/widgets.rs", "const", "CTRL_H", 60.0],
    "HERO_SCRIM_W": ["rust-modules/ui/src/widgets.rs", "const", "HERO_SCRIM_W", 1536.0],
    "HERO_SCRIM_TOP": ["rust-modules/ui/src/widgets.rs", "const", "HERO_SCRIM_TOP", 162.0],
    "HERO_SCRIM_R_TOP": ["rust-modules/ui/src/widgets.rs", "const", "HERO_SCRIM_R_TOP", 702.0],
    "HERO_LOGO_TOP_DETAIL": ["rust-modules/ui/src/widgets.rs", "literal", r"y=(\d+) on detail", 298.0],
    "HERO_LOGO_TOP_HOME": ["rust-modules/ui/src/hero_logo.rs", "literal", r"tall\.y,\s*([0-9.]+),", 260.0],
    "HERO_TEXT_W": ["rust-modules/ui/src/detail_layout.rs", "const", "HERO_TEXT_W", 943.0],
    "PEOPLE_W": ["rust-modules/ui/src/detail_layout.rs", "const", "PEOPLE_W", 560.0],
    "PEOPLE_LEAD": ["rust-modules/ui/src/detail_layout.rs", "const", "PEOPLE_LEAD", 32.0],
    "PEOPLE_MAX_LINES": ["rust-modules/ui/src/detail_layout.rs", "const", "PEOPLE_MAX_LINES", 4.0],
    # The formulas the zones rebuild from those numbers:
    "HOME_ROW_RULE": ["rust-modules/screens/src/home/mod.rs", "text",
                      "const HERO_ROW_Y: f32 = HERO_TEXT_BOTTOM + theme::space::MD;", None],
    "HOME_CTRL_RULE": ["rust-modules/screens/src/home/mod.rs", "text",
                       "const HERO_CTRL_D: f32 = StatusOverlay::CTRL_H;", None],
    "HOME_SHELF_HEADING_RULE": ["rust-modules/screens/src/home/mod.rs", "text", "row_y - TITLE_DY - lift", None],
    "HOME_SHELF_PEEK_RULE": ["rust-modules/screens/src/home/mod.rs", "text",
                             "let top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * self.snap.pos;", None],
    "DETAIL_PEOPLE_RULE": ["rust-modules/ui/src/detail_layout.rs", "text",
                           "Rect::new(SCR_W - MARGIN_X - PEOPLE_W, 700.0, PEOPLE_W, 100.0)", None],
    "DETAIL_TEXT_RULE": ["rust-modules/ui/src/detail_layout.rs", "text",
                         "Rect::new(MARGIN_X, TITLE_BOTTOM - 200.0, HERO_TEXT_W, 200.0)", None],
}


# The size of the app's rotating hero pool (`hero_pool`), pinned to the Rust constant the same way.
# Not a zone number, so it is checked with the pins but feeds no zone.
HERO_POOL_PINS = {"HERO_MAX": ["rust-modules/data/src/pms.rs", "const", "HERO_MAX", 8.0]}


def _rust_expr(expr, names):
    """The value of a Rust arithmetic expression (numbers, + - * /, names, `a::b::` paths and
    `as f32` casts) with each name looked up in `names`. Anything else is refused."""
    import ast
    src = re.sub(r"\bas\s+f32\b", "", re.sub(r"(?:[A-Za-z_]\w*::)+", "", expr)).strip()
    try:
        tree = ast.parse(src, mode="eval")
    except SyntaxError:
        raise AssertionError(f"not an arithmetic expression: {expr!r}")

    def ev(node):
        if isinstance(node, ast.Expression):
            return ev(node.body)
        if isinstance(node, ast.Constant) and type(node.value) in (int, float):
            return float(node.value)
        if isinstance(node, ast.Name):
            assert node.id in names, f"unknown name {node.id!r} in {expr!r}"
            return float(names[node.id])
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
            return -ev(node.operand)
        if isinstance(node, ast.BinOp) and type(node.op) in (ast.Add, ast.Sub, ast.Mult, ast.Div):
            a, b = ev(node.left), ev(node.right)
            return {ast.Add: a + b, ast.Sub: a - b, ast.Mult: a * b, ast.Div: a / b if b else float("nan")}[type(node.op)]
        raise AssertionError(f"not an arithmetic expression: {expr!r}")
    return round(ev(tree), 6)


def _rust_pin_value(root, pin, names):
    """What the Rust source says for one pin: a number, or (for `text`) True when the text is there.
    Raises AssertionError when the source no longer says it."""
    file, kind, key = pin[0], pin[1], pin[2]
    path = root / file
    assert path.is_file(), f"{file} does not exist"
    text = path.read_text()
    if kind == "text":
        assert key in text, f"{file} no longer contains `{key}`"
        return True
    if kind == "literal":
        found = re.findall(key, text)
        assert len(found) == 1, f"{file}: /{key}/ matches {len(found)} times, not once"
        return float(found[0])
    scope, name = text, key
    if "::" in key:  # `mod::NAME`: the constant inside `pub mod mod { ... }`
        mod, name = key.split("::", 1)
        m = re.search(rf"pub mod {mod}\s*\{{", text)
        assert m, f"{file} has no `pub mod {mod}`"
        scope = text[m.end():text.index("\n}", m.end())]
    found = re.findall(rf"\bconst\s+{re.escape(name)}\s*:\s*\w+\s*=\s*([^;]+);", scope)
    assert len(found) == 1, f"{file}: `const {name}` is declared {len(found)} times, not once"
    return _rust_expr(found[0], names)


def check_hero_pins(pins=None, root=None):
    """Re-read every pinned layout number from the Rust source and fail, naming each constant and
    its file, when a Python copy has drifted or the source no longer says what the zone is built on."""
    pins = {**HERO_PINS, **HERO_POOL_PINS} if pins is None else pins
    root = ROOT if root is None else pathlib.Path(root)
    names, drift = {}, []
    for name, pin in pins.items():
        try:
            got = _rust_pin_value(root, pin, names)
        except AssertionError as e:
            drift.append(f"{name}: {e} ({pin[0]})")
            continue
        if pin[1] == "text":
            continue
        names[name] = got
        if pin[1] == "const":
            names[pin[2].split("::")[-1]] = got  # an expression may use the constant's own name
        if abs(got - pin[3]) > 1e-3:
            drift.append(f"{name}: {pin[0]} says {got:g}, tools/demo_library.py HERO_PINS says {pin[3]:g}")
    assert not drift, ("the hero zones no longer match the layout; update HERO_PINS (and re-check the zone "
                       "list) from the Rust source:\n  " + "\n  ".join(drift))


def hero_zones(pins=None):
    """The rectangles ([x0, y0, x1, y1], frame pixels) hero art must keep its subject out of, built
    from the pinned layout numbers. `advisory` ones are reported, never gated."""
    v = {n: p[3] for n, p in (HERO_PINS if pins is None else pins).items()}
    w, h = v["SCR_W"], v["SCR_H"]
    text_r = v["MARGIN_X"] + v["HERO_TEXT_W"]
    people_l = w - v["MARGIN_X"] - v["PEOPLE_W"]

    def zone(name, rect, uses, **extra):
        return dict(name=name, rect=rect, uses=uses, **extra)
    return [
        # Home: the logo (paints up from the band to y 260), title, meta, synopsis and the CTA row
        # (HERO_ROW_Y = TEXT_BOTTOM + MD, CTRL_H tall), all in the COL_W column from the margin.
        zone("home text column", [v["MARGIN_X"], v["HERO_LOGO_TOP_HOME"], v["MARGIN_X"] + v["COL_W"],
                                  v["TEXT_BOTTOM"] + v["SPACE_MD"] + v["CTRL_H"]],
             ["MARGIN_X", "HERO_LOGO_TOP_HOME", "COL_W", "TEXT_BOTTOM", "SPACE_MD", "CTRL_H"]),
        # The peek shelf: its heading draws TITLE_DY above the row origin PEEK_Y, its cards below.
        zone("home shelf row", [0.0, v["PEEK_Y"] - v["TITLE_DY"], w, h], ["PEEK_Y", "TITLE_DY", "SCR_H", "SCR_W"]),
        # Detail: logo, identity line, synopsis and buttons, to the synopsis' wrap edge. The chain
        # below the title is measured at run time, so this runs to the foot of the frame.
        zone("detail text column", [v["MARGIN_X"], v["HERO_LOGO_TOP_DETAIL"], text_r, h],
             ["MARGIN_X", "HERO_LOGO_TOP_DETAIL", "HERO_TEXT_W", "SCR_H"]),
        # The facts row runs from there to the people column and sits level with that column's top
        # line (a 4-line block reaches PEOPLE_MAX_LINES * PEOPLE_LEAD below it).
        zone("detail facts row", [text_r, v["HERO_SCRIM_R_TOP"], people_l,
                                  v["HERO_SCRIM_R_TOP"] + v["PEOPLE_MAX_LINES"] * v["PEOPLE_LEAD"]],
             ["MARGIN_X", "HERO_TEXT_W", "SCR_W", "PEOPLE_W", "HERO_SCRIM_R_TOP", "PEOPLE_MAX_LINES", "PEOPLE_LEAD"]),
        # The right-aligned Starring block, from where the right wedge starts to the frame's foot.
        zone("detail starring column", [people_l, v["HERO_SCRIM_R_TOP"], w - v["MARGIN_X"], h],
             ["SCR_W", "MARGIN_X", "PEOPLE_W", "HERO_SCRIM_R_TOP", "SCR_H"]),
        # The left wedge's scrim: it darkens, it does not hide, so its share is only printed.
        zone("hero scrim", [0.0, v["HERO_SCRIM_TOP"], v["HERO_SCRIM_W"], h],
             ["HERO_SCRIM_TOP", "HERO_SCRIM_W", "SCR_H"], advisory=True),
    ]


def hero_map_box(src_w, src_h, recipe, box, frame=ART):
    """`box` ([x0, y0, x1, y1] in source pixels) where it lands in the derived frame for a `cover`
    recipe: scale by max(frame_w / src_w, frame_h / src_h), then crop the overflow at the anchor
    (`COVER_ANCHORS`). Pure arithmetic on the recorded size; ffmpeg's rounding is within a pixel."""
    assert recipe.get("mode", "cover") == "cover", "only a cover crop has this mapping"
    fw, fh = frame
    s = max(fw / src_w, fh / src_h)
    fx, fy = COVER_ANCHORS.get(recipe_anchor(recipe), (0.5, 0.5))
    ox, oy = (src_w * s - fw) * fx, (src_h * s - fh) * fy
    x0, y0, x1, y1 = box
    return [x0 * s - ox, y0 * s - oy, x1 * s - ox, y1 * s - oy]


def _overlap(a, b):
    """Area of the intersection of two [x0, y0, x1, y1] rectangles (0 when they only touch)."""
    w = min(a[2], b[2]) - max(a[0], b[0])
    h = min(a[3], b[3]) - max(a[1], b[1])
    return w * h if w > 1e-6 and h > 1e-6 else 0.0


def _is_box(subject):
    return (isinstance(subject, (list, tuple)) and len(subject) == 4
            and all(type(n) in (int, float) and n >= 0 for n in subject)
            and subject[2] > subject[0] and subject[3] > subject[1])


def hero_geometry(assets, rec):
    """The hero-art verdict for one title, from its manifests alone. `verdict` is `pass`, `fail` or
    `skip` (no art: that is a backdrop gap already); `reasons` says why it fails: `extend`/`mode`
    (not a cover crop), `no-subject`, `bad-subject`, `outside-source`, `outside-frame` (the crop cuts
    the subject off), `centre-left`, and `overlap:<zone>` per gated zone it touches."""
    out = {"verdict": "fail", "reasons": [], "box": None, "overlaps": {}, "centre_x": None,
           "scrim_share": None, "source": None}
    art = rec.get("art")
    if not art:
        return dict(out, verdict="skip", reasons=["no-art"])
    src = assets[art["asset"]]
    out["source"] = (src["width"], src["height"])
    mode = art.get("mode", "cover")
    if mode != "cover":
        out["reasons"] = ["extend" if mode == "extend" else "mode"]
        return out
    subject = art.get("subject")
    if subject is None:
        out["reasons"] = ["no-subject"]
    elif not _is_box(subject):
        out["reasons"] = ["bad-subject"]
    elif subject[2] > src["width"] or subject[3] > src["height"]:
        out["reasons"] = ["outside-source"]
    if out["reasons"]:
        return out
    box = out["box"] = hero_map_box(src["width"], src["height"], art, subject)
    fw, fh = ART
    if box[0] < -HERO_FRAME_EPS or box[1] < -HERO_FRAME_EPS or box[2] > fw + HERO_FRAME_EPS \
            or box[3] > fh + HERO_FRAME_EPS:
        out["reasons"] = ["outside-frame"]
        return out
    out["centre_x"] = (box[0] + box[2]) / 2
    if out["centre_x"] < HERO_CENTRE_MIN_X:
        out["reasons"].append("centre-left")
    area = (box[2] - box[0]) * (box[3] - box[1])
    for z in hero_zones():
        hit = _overlap(box, z["rect"])
        if z.get("advisory"):
            out["scrim_share"] = hit / area
        else:
            out["overlaps"][z["name"]] = hit
            if hit:
                out["reasons"].append(f"overlap:{z['name']}")
    out["verdict"] = "fail" if out["reasons"] else "pass"
    return out


def hero_eligible(catalog):
    """The titles the home hero can show, in catalog order: the pinned hero and its alternatives, the
    Continue Watching deck (an episode stands for its show) and Recently Added. A title taken out
    of all of them can never be a hero, which is the other way to satisfy the check."""
    pool = {catalog.get("hero")} | set(catalog.get("hero_alternatives", [])) | set(catalog.get("added_order", []))
    pool |= {c["item"].split("/")[0] for c in catalog.get("continue_watching", [])}
    return [t["id"] for t in catalog["movies"] + catalog.get("shows", [])
            if t["id"] in pool and not t.get("not_hero")]


def hero_pool(catalog, hero=None):
    """The titles the app's rotating hero holds on the mock's Home, in slot order: `merge` in
    `data/src/pms.rs` takes Continue Watching first (the hero at its head, as `CatalogLibrary` pins
    it), then Recently Added (newest first), skips an item with no landscape `art`, drops a repeat
    and stops at `HERO_MAX`. Only a movie qualifies: the mock's episode rows carry `grandparentArt`,
    which the app does not read, so an episode (Continue Watching's, Recently Added's) has no art
    and is skipped, and a show is never on a Home shelf. `not_hero` keeps a title out of the pool by
    its being outside these slots, which `check_flags` enforces and a test holds to the mock's Home."""
    movies = {m["id"]: m for m in catalog["movies"]}
    hero = hero or catalog["hero"]
    deck = [c["item"] for c in catalog.get("continue_watching", [])]
    if hero not in deck:
        deck.insert(0, hero)
    deck.sort(key=lambda ref: ref != hero)  # stable: the hero first, the rest in order
    pool = []
    for ref in deck + catalog.get("added_order", []):
        if ref in movies and "art" in movies[ref] and ref not in pool:
            pool.append(ref)
    return pool[:int(HERO_POOL_PINS["HERO_MAX"][3])]


def check_flags(assets, catalog, seen):
    """The per-title flags and approval lists (module docstring): `not_hero`, `not_in_video`,
    `poster.still_as_poster` + `still_poster_approved`, `logo_none_approved`."""
    films = {t["id"]: t for t in catalog["movies"] + catalog.get("shows", [])}
    pinned = [catalog["hero"], *catalog.get("hero_alternatives", [])]
    pool = hero_pool(catalog)
    for key, rec in films.items():
        for flag in ("not_hero", "not_in_video"):
            assert rec.get(flag, True) is True, f"{key}: {flag} is `true` or absent"
        if rec.get("not_hero"):
            where = [name for name, hit in (("the pinned hero", key == catalog["hero"]),
                                            ("hero_alternatives", key in catalog.get("hero_alternatives", [])),
                                            ("the hero pool's slots", key in pool)) if hit]
            assert not where, f"{key}: not_hero, yet it is in {', '.join(where)}"
        if rec.get("not_in_video"):
            assert key not in pinned, f"{key}: not_in_video, yet the video opens the pinned hero and its alternatives"
        poster = rec.get("poster", {})
        if "still_as_poster" in poster:
            assert poster["still_as_poster"] is True, f"{key}: still_as_poster is `true` or absent"
            assert key in catalog.get("still_poster_approved", []), \
                f"{key}: a still as the poster needs the owner's approval (`still_poster_approved`)"
            assert assets[poster["asset"]]["kind"] in ("still", "backdrop"), \
                f"{key}: still_as_poster names a still or a backdrop, not a {assets[poster['asset']]['kind']}"
    for name in ("still_poster_approved", "logo_none_approved"):
        for key in catalog.get(name, []):
            assert key in films, f"{name}: no title {key!r}"
    for key in catalog.get("still_poster_approved", []):
        assert films[key].get("poster", {}).get("still_as_poster") is True, \
            f"{key}: approved for a still as its poster, so the poster says `still_as_poster`"
    for key in catalog.get("logo_none_approved", []):
        assert films[key].get("not_in_video") is True, \
            f"{key}: it has no clear logo (logo_none_approved), so it is never opened in the video: not_in_video"


def hero_gap(assets, catalog, rec):
    """The reasons `rec` fails the hero check; empty when it passes, has no art or is not eligible."""
    if rec["id"] not in hero_eligible(catalog):
        return []
    v = hero_geometry(assets, rec)
    return v["reasons"] if v["verdict"] == "fail" else []


def hero_report(assets, catalog):
    """Print the zones with the Rust constants they are pinned to, then one line per hero-eligible
    title: its art recipe, the declared subject, where the box lands, the area it shares with each
    gated zone, and the verdict."""
    print("hero zones (frame pixels x0 y0 x1 y1); each number is pinned to the Rust source in HERO_PINS:")
    for z in hero_zones():
        tag = " (advisory)" if z.get("advisory") else ""
        print(f"  {z['name']:<24} {' '.join(f'{n:g}' for n in z['rect'])}{tag}")
    print("pins (python copy == Rust source, checked by `check --complete`):")
    for name, (file, kind, key, value) in HERO_PINS.items():
        print(f"  {name:<24} {'' if value is None else f'{value:g}':<7} {file} {kind} {key}")
    print(f"the subject's centre must be at x >= {HERO_CENTRE_MIN_X:g}; `extend` art is refused\n")
    counts = {"pass": 0, "fail": 0, "skip": 0}
    films = {t["id"]: t for t in catalog["movies"] + catalog.get("shows", [])}
    for key in hero_eligible(catalog):
        rec = films[key]
        v = hero_geometry(assets, rec)
        counts[v["verdict"]] += 1
        art = rec.get("art", {})
        src = "-" if v["source"] is None else "{}x{}".format(*v["source"])
        recipe = f"{art.get('mode', 'cover')}/{recipe_anchor(art)}" if art else "-"
        subject = json.dumps(art.get("subject")) if art.get("subject") is not None else "-"
        box = "-" if v["box"] is None else "[" + ", ".join(f"{n:.0f}" for n in v["box"]) + "]"
        hits = ", ".join(f"{n} {a:.0f}px2" for n, a in v["overlaps"].items() if a) or "-"
        scrim = "-" if v["scrim_share"] is None else f"{v['scrim_share']:.0%}"
        print(f"{key:<26} {src:>10} {recipe:<14} subject {subject} -> {box} overlaps {hits} "
              f"scrim {scrim} {v['verdict']} {', '.join(v['reasons']) or '-'}")
    print(f"\nhero art: {counts['pass']} pass, {counts['fail']} fail, {counts['skip']} skip (no art)")


# ------------------------------------------------------------------ completeness -------------

# What a source file IS (`kind` in assets.json), which decides the roles it may fill: a title's
# poster is a `poster`, its backdrop a `backdrop` or a `still`, an episode's thumbnail a `still`.
# `media` is a film itself, `headshot` a photograph of a person (none yet), `logo-title-card` a
# film's own official title card or a frame of the film, the source of its clear logo.
ASSET_KINDS = frozenset({"poster", "backdrop", "still", "headshot", "media", "logo-title-card"})
# Where a `logo-title-card` comes from: the studio's own title card, or a frame of the film itself.
TITLE_CARD_ORIGINS = frozenset({"official-title-card", "film-frame"})
# The licences the demo library accepts, exactly. Share-alike is not among them: it would put the
# credits page's whole body under the same terms (owner decision pending).
LICENCES = frozenset({"CC0 1.0", "CC0", "Public domain", "CC BY 3.0", "CC BY 4.0"})
# The categories a gap is reported (and listed in the pending file) under.
GAP_CATEGORIES = frozenset({
    "title", "year", "summary", "genres", "directors", "creators", "tagline", "cast", "writers",
    "country", "poster", "backdrop", "hero_art", "logo", "still"})
PENDING = ROOT / "tests" / "demo_library" / "pending.json"


def _is_cited(row):
    return (isinstance(row, dict) and str(row.get("source", "")).startswith("https://")
            and bool(DATE.match(str(row.get("retrieved", "")))))


def _asset_kind(assets, ref):
    """The kind of the asset a `{"asset": id}` recipe names, or None."""
    return assets.get(ref.get("asset"), {}).get("kind") if isinstance(ref, dict) else None


def _gaps_of(assets, catalog, kind, rec):
    gaps = set()
    if not (isinstance(rec.get("title"), str) and rec["title"]):
        gaps.add("title")
    if kind == "episode":
        if not rec.get("summary"):
            gaps.add("summary")
        if _asset_kind(assets, rec.get("thumb")) != "still":
            gaps.add("still")
        return gaps
    if not isinstance(rec.get("year"), int):
        gaps.add("year")
    if not rec.get("summary"):
        gaps.add("summary")
    if not rec.get("genres"):
        gaps.add("genres")
    credit = "creators" if kind == "show" else "directors"
    if not rec.get(credit):
        gaps.add(credit)
    if not (rec.get("tagline") and "tagline" in rec.get("demo_values", [])):
        gaps.add("tagline")
    if not (rec.get("cast") or _is_cited(rec.get("cast_none"))):
        gaps.add("cast")
    # A show's creators are the writers its page shows ("Created by"), so they count as listed.
    if not (rec.get("writers") or (kind == "show" and rec.get("creators")) or _is_cited(rec.get("writers_none"))):
        gaps.add("writers")
    if not rec.get("countries"):
        gaps.add("country")
    # A film still may stand in for the poster only where the owner approved it for this title and the
    # poster says so (`check_flags` ties the two together).
    poster_kind = _asset_kind(assets, rec.get("poster"))
    still_ok = (poster_kind in ("still", "backdrop") and rec["poster"].get("still_as_poster") is True
                and rec["id"] in catalog.get("still_poster_approved", []))
    if poster_kind != "poster" and not still_ok:
        gaps.add("poster")
    if _asset_kind(assets, rec.get("art")) not in ("backdrop", "still"):
        gaps.add("backdrop")
    # Art a hero can show must also put its subject on the right, clear of the text (`hero_geometry`).
    if hero_gap(assets, catalog, rec):
        gaps.add("hero_art")
    # A clearLogo is cut from the item's own poster; only an owner-approved title may go without.
    logo = rec.get("logo")
    cut_from_poster = (isinstance(logo, dict) and logo.get("asset") == rec.get("poster", {}).get("asset")
                       and _asset_kind(assets, logo) == "poster")
    # ...or it is the film's own title card (`check` has already tied its `of` to this title).
    own_card = (isinstance(logo, dict) and _asset_kind(assets, logo) == "logo-title-card"
                and assets[logo["asset"]].get("of") == rec["id"])
    if not (cut_from_poster or own_card or rec["id"] in catalog.get("logo_none_approved", [])):
        gaps.add("logo")
    return gaps


def complete_gaps(assets, catalog):
    """{item key: sorted gap categories} for every title and episode that is not yet complete,
    judged from the manifests alone (no file is opened). The rule is S2 of the demo-library plan;
    `check_cited_metadata` already refuses a value that is present but uncited."""
    out = {}
    for key, kind, rec in item_keys(catalog):
        gaps = _gaps_of(assets, catalog, kind, rec)
        if gaps:
            out[key] = sorted(gaps)
    return out


def check_assets_complete(assets):
    """Per source file: an accepted licence, and no headshot that carries the Commons
    personality-rights tag (a person's likeness is not covered by the photograph's licence). A
    headshot must record its `tags`: an absent list is "not looked at", never "clear"."""
    for aid, a in sorted(assets.items()):
        assert a["licence"] in LICENCES, \
            f"asset {aid}: licence {a['licence']!r} is not one of {sorted(LICENCES)}"
        if a["kind"] == "headshot":
            assert isinstance(a.get("tags"), list), f"asset {aid}: a headshot records the file page's `tags`"
            assert not any("personality" in str(t).lower() for t in a["tags"]), \
                f"asset {aid}: a headshot with the personality-rights tag is refused"


def load_pending():
    """{item key: sorted gap categories} the committed `pending.json` accepts for now."""
    return json.loads(PENDING.read_text())["pending"]


def check_complete(assets, catalog, pending=None):
    """The completeness gate. The catalog may hold only what `pending` lists: a gap that is not on
    the list fails (new content must be complete), and so does a listed gap that no longer exists
    (the entry is deleted when its content lands, so the list can only shrink). Returns the gaps."""
    pending = load_pending() if pending is None else pending
    check_hero_pins()
    check_assets_complete(assets)
    gaps = complete_gaps(assets, catalog)
    new = {k: sorted(set(v) - set(pending.get(k, []))) for k, v in gaps.items()}
    new = {k: v for k, v in new.items() if v}
    assert not new, f"incomplete and not pending (fill them in; never add to the pending list): {new}"
    stale = {k: sorted(set(v) - set(gaps.get(k, []))) for k, v in pending.items()}
    stale = {k: v for k, v in stale.items() if v}
    assert not stale, f"pending entries that are complete now (delete them from {PENDING.name}): {stale}"
    return gaps


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
            recoloured = any("fill" in k for k in rec["logo"].get("keys", []))
            card = assets[rec["logo"]["asset"]]["kind"] == "logo-title-card"
            where.setdefault(rec["logo"]["asset"], []).append(
                (key, "logo-card" if card else "logo-fill" if recoloured else "logo"))
    label = {"poster": "poster", "art": "backdrop", "thumb": "episode still", "media": "video",
             "logo-card": "clear logo, the film's own title card",
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
        "Where the table says a logo is the film's own title card, it is that card (or a frame of the",
        "film) trimmed to its lettering, under its own licence.",
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
    if role == "logo-card":
        return "The film's own title card, trimmed to its lettering for the clear logo; nothing redrawn."
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
                         "logo": "Clear logo", "logo-card": "Clear logo"}[role]
                if label not in u["roles"]:
                    u["roles"].append(label)
                change = _changes(role, recipe)
                if change not in u["changes"]:
                    u["changes"].append(change)
                if episode:
                    u["episodes"].append(episode)

            for role in ("poster", "art", "thumb", "logo"):
                if role in rec:
                    card = role == "logo" and assets[rec[role]["asset"]]["kind"] == "logo-title-card"
                    use(rec[role]["asset"], "logo-card" if card else role, rec[role])
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
    <link rel="icon" type="image/png" sizes="32x32" href="icons/favicon-32.png" />
    <link rel="icon" type="image/png" sizes="48x48" href="icons/favicon-48.png" />
    <link rel="apple-touch-icon" sizes="180x180" href="icons/apple-touch-icon.png" />
    <meta name="theme-color" content="#202022" />
    <meta property="og:type" content="website" />
    <meta property="og:site_name" content="PlxNative" />
    <meta property="og:url" content="https://plxnative.com/credits.html" />
    <meta property="og:title" content="Artwork credits — PlxNative" />
    <meta property="og:description" content="Credits and licences for the openly licensed artwork in the PlxNative screenshots." />
    <meta property="og:image" content="https://plxnative.com/media/og-card.jpg" />
    <meta property="og:image:type" content="image/jpeg" />
    <meta property="og:image:width" content="1200" />
    <meta property="og:image:height" content="630" />
    <meta property="og:image:alt" content="PlxNative home screen on an LG TV, next to the headline: Plex that feels fast on LG TVs." />
    <meta property="og:locale" content="en_US" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:title" content="Artwork credits — PlxNative" />
    <meta name="twitter:description" content="Credits and licences for the openly licensed artwork in the PlxNative screenshots." />
    <meta name="twitter:image" content="https://plxnative.com/media/og-card.jpg" />
    <meta name="twitter:image:alt" content="PlxNative home screen on an LG TV, next to the headline: Plex that feels fast on LG TVs." />
    <link rel="stylesheet" href="styles.css" />
  </head>
  <body>
    <div class="page-shell">
      <div class="ambient-ground" aria-hidden="true"></div>
      <main class="page-root">
        <header class="site-header">
          <div class="header-bar">
            <a class="brand" href="./" aria-label="Back to PlxNative">
              <span class="brand-mark"><img src="icons/brand-mark.png" alt="" width="22" height="22" /></span>
              <span class="brand-name"><span class="back-arrow" aria-hidden="true">&larr;</span> PlxNative</span>
            </a>
            <nav class="site-nav" aria-label="Primary navigation">
              <a href="./#feel">Demo</a>
              <a href="./#why">Why</a>
              <a href="./#showcase">Features</a>
            </nav>
            <div class="header-actions">
              <a class="header-github" href="https://github.com/GLinnik21/plx-native" aria-label="PlxNative on GitHub">
                <svg class="gh-icon" viewBox="0 0 24 24" width="21" height="21" fill="currentColor" aria-hidden="true"><path d="M12 1.5a10.5 10.5 0 0 0-3.32 20.46c.53.1.72-.23.72-.5v-1.76c-2.92.64-3.54-1.4-3.54-1.4-.48-1.22-1.17-1.55-1.17-1.55-.95-.65.07-.64.07-.64 1.06.08 1.61 1.09 1.61 1.09.94 1.6 2.46 1.14 3.06.87.1-.68.37-1.14.67-1.4-2.33-.27-4.78-1.17-4.78-5.2 0-1.15.41-2.09 1.08-2.83-.11-.27-.47-1.34.1-2.79 0 0 .88-.28 2.88 1.08a9.98 9.98 0 0 1 5.24 0c2-1.36 2.88-1.08 2.88-1.08.57 1.45.21 2.52.1 2.79.67.74 1.08 1.68 1.08 2.83 0 4.04-2.46 4.93-4.8 5.19.38.33.72.97.72 1.96v2.9c0 .28.19.62.73.51A10.5 10.5 0 0 0 12 1.5Z"></path></svg>
              </a>
              <a class="header-kofi" href="https://ko-fi.com/0xbeb" aria-label="Support the project on Ko-fi" title="Support the project on Ko-fi">
                <svg class="kofi-icon" viewBox="-1 0 242 194" width="25" height="20" fill="currentColor" aria-hidden="true"><mask id="kofi-cut" maskUnits="userSpaceOnUse" x="-1" y="0" width="242" height="194"><rect x="-1" y="0" width="242" height="194" fill="white"></rect><path d="M15.1975 67.7674C15.1975 37.5285 33.3866 21.164 54.7559 18.4334C70.8987 16.387 90.906 16.1589 114.544 16.1589C151.372 16.1589 160.919 16.6151 174.559 17.9772C206.617 21.1576 225.255 40.937 225.255 69.3577V72.9941C225.255 99.3687 205.932 120.966 179.786 123.234C177.74 130.058 174.559 136.874 170.238 143.698C160.235 159.156 140.228 178.707 103.4 178.707H96.1264C66.1155 178.707 42.9277 165.751 29.0595 142.107C16.7814 121.422 15.1912 98.4563 15.1912 67.7674" fill="black"></path><path d="M32.2469 67.9899C32.2469 97.3168 34.0654 116.184 43.6127 133.689C54.5225 153.924 74.3018 161.653 96.8117 161.653H103.857C133.411 161.653 147.736 147.329 155.693 134.829C159.558 128.462 162.966 121.417 164.784 112.547L166.147 106.864H174.332C192.521 106.864 208.208 92.09 208.208 73.2166V69.8082C208.208 48.6669 195.024 37.5228 172.058 34.7987C159.102 33.6646 151.372 33.2084 114.538 33.2084C89.7602 33.2084 72.0272 33.4364 58.6152 35.4828C39.7483 38.2134 32.2407 48.8951 32.2407 67.9899" fill="white"></path><path d="M166.158 83.6801C166.158 86.4107 168.204 88.4572 171.841 88.4572C183.435 88.4572 189.802 81.8619 189.802 70.9523C189.802 60.0427 183.435 53.2195 171.841 53.2195C168.204 53.2195 166.158 55.2657 166.158 57.9963V83.6866V83.6801Z" fill="black"></path><path d="M54.5321 82.3198C54.5321 95.732 62.0332 107.326 71.5807 116.424C77.9478 122.562 87.9515 128.93 94.7685 133.022C96.8147 134.157 98.8611 134.841 101.136 134.841C103.866 134.841 106.134 134.157 107.959 133.022C114.782 128.93 124.779 122.562 130.919 116.424C140.694 107.332 148.195 95.7383 148.195 82.3198C148.195 67.7673 137.286 54.8115 121.599 54.8115C112.28 54.8115 105.912 59.5882 101.136 66.1772C96.8147 59.582 90.2259 54.8115 80.9001 54.8115C64.9855 54.8115 54.5256 67.7673 54.5256 82.3198" fill="black"></path></mask><path d="M96.1344 193.911C61.1312 193.911 32.6597 178.256 15.9721 149.829C1.19788 124.912 -0.00585938 97.9229 -0.00585938 67.7662C-0.00585938 49.8876 5.37293 34.3215 15.5413 22.7466C24.8861 12.1157 38.1271 5.22907 52.8317 3.35378C70.2858 1.14271 91.9848 0.958984 114.545 0.958984C151.259 0.958984 161.63 1.4088 176.075 2.85328C195.29 4.76026 211.458 11.932 222.824 23.5955C234.368 35.4428 240.469 51.2624 240.469 69.3627V72.9994C240.469 103.885 219.821 129.733 191.046 136.759C188.898 141.827 186.237 146.871 183.089 151.837L183.006 151.964C172.869 167.632 149.042 193.918 103.401 193.918H96.1281L96.1344 193.911Z" mask="url(#kofi-cut)"></path></svg>
              </a>
              <a class="header-cta" href="/#install">Install</a>
            </div>
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


FIXTURES = ROOT / "tests" / "demo_library" / "fixtures"
FIXTURE_TITLES = {"sintel": 102, "tears-of-steel": 105}  # the two fixture titles, by ratingKey


def fixtures(dst=FIXTURES):
    """Capture the mock's `/library/metadata/<rk>` answer for each fixture title, the detail
    pages the data crate parses (`metadata_demo_fixture_tests.rs`). Needs the derived artwork."""
    sys.path.insert(0, str(ROOT / "tests"))
    import mock_pms
    pms = mock_pms.MockPms(mock_pms.CatalogLibrary(CATALOG))
    dst.mkdir(parents=True, exist_ok=True)
    for slug, rk in FIXTURE_TITLES.items():
        status, _, body = pms.handle("GET", f"/library/metadata/{rk}")
        assert status == 200, (slug, status)
        (dst / f"detail-{rk}.json").write_text(json.dumps(json.loads(body), indent=1, sort_keys=True) + "\n")
        print(f"demo_library: wrote {dst / f'detail-{rk}.json'}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("command", choices=["fetch", "derive", "credits", "site-credits", "check", "hero-report",
                                        "fixtures"])
    ap.add_argument("--complete", action="store_true",
                    help="with `check`: also require every title and episode to be complete "
                         "(tests/demo_library/pending.json lists what is not yet)")
    a = ap.parse_args()
    if a.complete and a.command != "check":
        ap.error("--complete belongs to `check`")
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
    elif a.command == "fixtures":
        fixtures()
    elif a.command == "hero-report":
        hero_report(assets, catalog)
    else:
        print("demo_library: manifests ok")
        if a.complete:
            gaps = check_complete(assets, catalog)
            counts = {}
            for fields in gaps.values():
                for f in fields:
                    counts[f] = counts.get(f, 0) + 1
            by = ", ".join(f"{f} {n}" for f, n in sorted(counts.items())) or "none"
            print(f"demo_library: complete except the pending list: {len(gaps)} of "
                  f"{len(list(item_keys(catalog)))} items pending ({by})")


if __name__ == "__main__":
    main()
