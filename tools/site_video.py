#!/usr/bin/env python3
"""The host half of the site demo video: turn a finished master into the deliverables, grade them,
and adopt them into `site/media/`.

    python3 tools/site_video.py ffmpeg-fetch                   # the pinned static GPL ffmpeg; prints its path
    python3 tools/site_video.py encode MASTER.mkv frames.tsv --out DIR [--render render.json]
    python3 tools/site_video.py gates DIR --master MASTER.mkv --frames-b second-run/frames.tsv
    python3 tools/site_video.py scan-sentinel --size 1920x1080 [--tee] [FILE|-]   # RGB24 stream scanner
    python3 tools/site_video.py contact-sheet MASTER.mkv --out sheet.jpg [-n 12]
    python3 tools/site_video.py manifest DIR [--zip artifact.zip]   # feel.manifest.json (after `gates`)
    python3 tools/site_video.py verify DIR|feel.manifest.json
    python3 tools/site_video.py adopt artifact.zip [--write]        # dry run unless --write

Plan: the demo-video pipeline's stages S4 (the sentinel), S5b (the writer, which produces the INPUT
contract below) and S6/S7 (this file). Nothing here renders: it consumes a master and writes files.

THE INPUT CONTRACT (what the S5b dump writer must produce)

* `master.mkv`: FFV1 (`-level 3 -g 1`), RGB, 1920x1080, 60 fps, one packet per WRITTEN frame, no audio.
* `frames.tsv`: UTF-8, LF, a header line exactly `n<TAB>t_ms<TAB>xxh3<TAB>debt<TAB>holds`, then one
  row per written frame: `n` the written-frame index (0, 1, 2, ... with no gap), `t_ms` the virtual
  time T(n) in integer ms, `xxh3` the 16-hex-digit XXH3-64 of the frame's RGB24 bytes, `debt` the
  draw-site placeholder count at that frame (must be 0), `holds` how many held repeats preceded it
  (informational). The row count equals the master's frame count.
* `render.json`, the render record: see `RENDER_RECORD_FIELDS` and `validate_render_record`.

THE OUTPUTS: the four encodes and two posters named in `site/index.html` and `site/styles.css`
(`VIDEOS`, `POSTERS`), `frames.tsv`, `render.json`, `gates.json`, a contact sheet, and
`feel.manifest.json`. Only the six media files and the manifest are ever copied into the site.

THE TOOL FFMPEG is a pinned static GPL build (`PINS`), fetched by URL with a pinned sha256, failing
closed on a mismatch. It is a build tool and is not shipped in the app: the app's own FFmpeg is
LGPL and cannot encode (`ci/build-ffmpeg.sh` has `--disable-everything`). Nothing falls back to
libaom or to a system ffmpeg: `--ffmpeg PATH` is an explicit, loudly non-canonical override.
"""
import argparse
import collections
import hashlib
import io
import json
import operator
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SITE_MEDIA = ROOT / "site" / "media"
SITE_INDEX = ROOT / "site" / "index.html"
CATALOG = ROOT / "tests" / "demo_library" / "catalog.json"
STORYBOARD = ROOT / "tests" / "video" / "feel.json"
UA = "PlxNativeSiteVideo/1.0 (+https://github.com/GLinnik21/plex-native-poc)"
SCHEMA = 1


class Failure(Exception):
    """A refusal with a message fit to print as the whole error."""


# ------------------------------------------------------------------ what the site uses ----------

Video = collections.namedtuple("Video", "name codec width height")
Poster = collections.namedtuple("Poster", "name width height")

FPS = 60
# Mirrors the four <source> elements of `#feel` in site/index.html, in their order there (a test reads
# the HTML and compares): phones (max-width 759px) take a 720p encode, everything else 1080p, and the
# browser takes the first source whose `codecs` it can decode.
VIDEOS = (
    Video("feel-720p60.av1.mp4", "av1", 1280, 720),
    Video("feel-720p60.h264.mp4", "h264", 1280, 720),
    Video("feel-1080p60.av1.mp4", "av1", 1920, 1080),
    Video("feel-1080p60.h264.mp4", "h264", 1920, 1080),
)
# `feel-poster.jpg` is the CSS background of the frame (site/styles.css), the narrow one the phone
# variant. Both are frame 0 of the master.
POSTERS = (Poster("feel-poster.jpg", 1920, 1080), Poster("feel-poster-narrow.jpg", 960, 540))
MEDIA_NAMES = tuple(v.name for v in VIDEOS) + tuple(p.name for p in POSTERS)
MANIFEST_NAME = "feel.manifest.json"
MASTER_SIZE = (1920, 1080)

# ------------------------------------------------------------------ thresholds ------------------
# One table. CALIBRATED ON THE FIRST RENDER, THEN FROZEN: the numbers marked "spec" come from the plan
# (docs: S6); the ones marked "provisional" have no number in the plan, were picked here, and are to be
# set from the first real render's measurements and then not touched.
THRESHOLDS = {
    "ssim_min": 0.97,            # spec: per-frame minimum, each deliverable against the master
    "ssim_mean": 0.985,          # spec: mean over frames
    "size_ratio": 1.25,          # spec: each file <= 1.25x today's file of the same name
    "seam_ssim": 0.995,          # spec: SSIM(first frame, last frame)
    "vmaf_mean": 93.0,           # provisional (only checked where libvmaf is present)
    "vmaf_min": 80.0,            # provisional
    "banding_excess": 0.10,      # provisional: flat-pair fraction of the encode minus the master's
    # provisional: the scrim's dark region as fractions (x, y, w, h) of the frame; to be set to the real
    # box on the first render.
    "banding_roi": (0.0, 0.62, 0.35, 0.30),
    "banding_every": 30,         # probe every Nth frame
    "poster_ssim_min": 0.97,     # the poster is frame 0 and passes the same bar
}

# ------------------------------------------------------------------ the pinned tool ffmpeg -------
Pin = collections.namedtuple("Pin", "key urls sha256 kind member version licence source")

# Each build was DOWNLOADED and hashed by the author of this table on 2026-10-08, and the hash
# agrees with the publisher's own (GitHub's asset digest; the build server's .sha256 file).
#  * linux-x86_64 is the canonical build (CI). BtbN/FFmpeg-Builds, GPLv3 (`--enable-gpl
#    --enable-version3`), static libs, with libsvtav1, libx264, libaom and libvmaf; the build
#    scripts are public at github.com/BtbN/FFmpeg-Builds. Its URL is a dated `autobuild-*` release,
#    and BtbN prunes old autobuilds: when it 404s, mirror THIS exact tarball (same sha256) as a release
#    asset of this repository, add that URL to `urls`, and nothing else changes.
#  * darwin-arm64 is for local previews only (`adopt` refuses anything not rendered on linux-ci).
#    Martin Riedl's build server, GPLv3, ffmpeg 9.0.2, with libsvtav1 4.2.0, libx264 and libvmaf;
#    build script github.com/ffmpeg-build-tools. Permanent timestamped URL; I ran this binary.
#  * Rejected: johnvansickle.com's amd64 static 7.0.2 (GPLv3, durable) has libaom but NO libsvtav1.
PINS = {
    "linux-x86_64": Pin(
        "linux-x86_64",
        ("https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-07-13-07/"
         "ffmpeg-n9.0.2-22-g46d8f462ee-linux64-gpl-9.0.tar.xz",),
        "c03023a986dec781a5ab6bb8821d6544b40eb86e96f34283880a0fae8105da67",
        "tar.xz", "/bin/ffmpeg", "n9.0.2-22-g46d8f462ee", "GPL-3.0-or-later (--enable-gpl --enable-version3)",
        "BtbN/FFmpeg-Builds autobuild-2026-10-07-13-07"),
    "darwin-arm64": Pin(
        "darwin-arm64",
        ("https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip",),
        "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924",
        "zip", "ffmpeg", "9.0.2", "GPL-3.0-or-later (--enable-gpl --enable-version3)",
        "ffmpeg.martin-riedl.de macOS arm64 release 9.0.2 (1789931890)"),
}
REQUIRED_ENCODERS = ("libsvtav1", "libx264")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def cache_dir():
    env = os.environ.get("PLXNATIVE_DEMO_CACHE")
    return pathlib.Path(env) if env else pathlib.Path.home() / ".cache" / "plxnative-demo"


def host_pin_key():
    system, machine = platform.system(), platform.machine().lower()
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "linux-x86_64"
    if system == "Darwin" and machine in ("arm64", "aarch64"):
        return "darwin-arm64"
    raise Failure(f"no pinned ffmpeg build for {system}/{machine}: pass --ffmpeg PATH (non-canonical)")


def parse_encoders(text):
    """The encoder names `ffmpeg -encoders` lists (video/audio/subtitle flag column first)."""
    return {m.group(1) for m in re.finditer(r"^\s*[VAS][A-Z.]{5}\s+(\S+)\s", text, re.M)}


def assert_encoders(ffmpeg, run=subprocess.run):
    out = run([str(ffmpeg), "-hide_banner", "-encoders"], capture_output=True, text=True, timeout=60)
    have = parse_encoders(out.stdout)
    missing = [e for e in REQUIRED_ENCODERS if e not in have]
    if missing:
        raise Failure(f"{ffmpeg}: `-encoders` lacks {', '.join(missing)}"
                      + ("; libaom-av1 is present but this tool never falls back to it" if "libaom-av1" in have else ""))
    return have


def _download(pin, dest, opener=urllib.request.urlopen, sleep=time.sleep):
    """Stream the first URL that answers into `dest` and return the sha256 it hashes to. A hash that
    differs is returned, not retried: the caller fails closed. 429/5xx back off and retry (not a mismatch)."""
    last = None
    for url in pin.urls:
        for attempt in range(4):
            try:
                req = urllib.request.Request(url, headers={"User-Agent": UA})
                h = hashlib.sha256()
                with opener(req, timeout=120) as r, open(dest, "wb") as f:
                    for block in iter(lambda: r.read(1 << 20), b""):
                        h.update(block)
                        f.write(block)
                return h.hexdigest()
            except urllib.error.HTTPError as e:
                last = f"{url}: HTTP {e.code}"
                if e.code == 429 or e.code >= 500:
                    sleep(2 ** attempt)
                    continue
                break  # 404 and friends: the next URL
            except (urllib.error.URLError, OSError) as e:
                last = f"{url}: {e}"
                sleep(2 ** attempt)
    raise Failure(f"ffmpeg-fetch: no pinned URL answered (last: {last}). A dated BtbN autobuild is pruned "
                  f"eventually: mirror the pinned tarball ({pin.sha256}) and add its URL to PINS.")


def _extract_member(pin, archive, dest):
    want = pin.member
    if pin.kind == "zip":
        with zipfile.ZipFile(archive) as z:
            for info in z.infolist():
                if info.filename == want:
                    with z.open(info) as src, open(dest, "wb") as out:
                        shutil.copyfileobj(src, out)
                    return
    else:
        with tarfile.open(archive, "r:xz") as t:
            for m in t:
                if m.isfile() and m.name.endswith(want):
                    src = t.extractfile(m)
                    with open(dest, "wb") as out:
                        shutil.copyfileobj(src, out)
                    return
    raise Failure(f"ffmpeg-fetch: no `{want}` in the downloaded archive")


def fetch_ffmpeg(pin=None, cache=None, opener=urllib.request.urlopen, run=subprocess.run, sleep=time.sleep):
    """The pinned tool ffmpeg: a cached, re-verified binary, or a download checked against the pinned
    sha256 (fail closed) whose `-encoders` must list libsvtav1 and libx264. Returns its path."""
    pin = pin or PINS[host_pin_key()]
    root = pathlib.Path(cache) if cache else cache_dir()
    dest_dir = root / "tools" / f"ffmpeg-{pin.sha256[:12]}"
    binary, record = dest_dir / "ffmpeg", dest_dir / "verified.json"
    if binary.is_file() and record.is_file():
        try:
            ok = json.loads(record.read_text())
            if ok.get("archive_sha256") == pin.sha256 and ok.get("binary_sha256") == sha256_file(binary):
                assert_encoders(binary, run)
                return binary
        except (ValueError, Failure):
            pass
        shutil.rmtree(dest_dir, ignore_errors=True)
    dest_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=dest_dir) as tmp:
        archive = pathlib.Path(tmp) / "archive"
        got = _download(pin, archive, opener, sleep)
        if got != pin.sha256:
            raise Failure(f"ffmpeg-fetch: sha256 {got} != pinned {pin.sha256} ({pin.source}); refusing")
        part = pathlib.Path(tmp) / "ffmpeg"
        _extract_member(pin, archive, part)
        part.chmod(0o755)
        try:
            assert_encoders(part, run)
        except Failure:
            shutil.rmtree(dest_dir, ignore_errors=True)
            raise
        record.write_text(json.dumps({"archive_sha256": pin.sha256, "binary_sha256": sha256_file(part),
                                      "source": pin.source, "licence": pin.licence}, indent=1))
        os.replace(part, binary)
    return binary


def resolve_ffmpeg(explicit=None, cache=None, run=subprocess.run):
    """(path, canonical). An explicit path is allowed and loudly non-canonical; otherwise ONLY the
    already-fetched pinned build is used, never a system ffmpeg."""
    if explicit:
        path = pathlib.Path(explicit)
        if not path.is_file():
            raise Failure(f"--ffmpeg {explicit}: no such file")
        assert_encoders(path, run)
        print(f"site_video: WARNING: {path} is NOT the pinned tool ffmpeg; its output is a preview and "
              f"`adopt` will refuse it", file=sys.stderr)
        return path, False
    pin = PINS[host_pin_key()]
    root = pathlib.Path(cache) if cache else cache_dir()
    binary = root / "tools" / f"ffmpeg-{pin.sha256[:12]}" / "ffmpeg"
    if not binary.is_file():
        raise Failure("no pinned ffmpeg fetched yet: run `python3 tools/site_video.py ffmpeg-fetch` "
                      "(or pass --ffmpeg PATH for a non-canonical preview). A system ffmpeg is never used silently.")
    return fetch_ffmpeg(pin, cache, run=run), True


def ffmpeg_version_line(ffmpeg, run=subprocess.run):
    out = run([str(ffmpeg), "-hide_banner", "-version"], capture_output=True, text=True, timeout=60).stdout
    return out.splitlines()[0] if out else "unknown"


# ------------------------------------------------------------------ encode ----------------------
SCALE_FLAGS = "lanczos+accurate_rnd+full_chroma_int+error_diffusion"  # error_diffusion: the RGB->YUV dither
COLOR_TAGS = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]


def video_filter(width, height):
    """RGB master -> 8-bit yuv420p, BT.709, TV range, with dither. This chain is also the reference the
    gates compare against, so they measure the codec's loss and not the colour conversion's."""
    return (f"scale={width}:{height}:flags={SCALE_FLAGS}:in_range=full:out_range=tv:out_color_matrix=bt709,"
            f"format=yuv420p")


def poster_filter(width, height):
    """JPEG is BT.601 full range for every browser (today's posters are bt470bg/pc)."""
    return (f"scale={width}:{height}:flags={SCALE_FLAGS}:in_range=full:out_range=pc:out_color_matrix=bt601,"
            f"format=yuvj420p")


AV1_CODEC = ["-c:v", "libsvtav1", "-preset", "4", "-crf", "30", "-g", "120",
             "-svtav1-params", "tune=0:film-grain=0", "-pix_fmt", "yuv420p"]
H264_CODEC = ["-c:v", "libx264", "-preset", "veryslow", "-crf", "18", "-tune", "animation",
              "-profile:v", "high", "-pix_fmt", "yuv420p", "-x264-params", "aq-mode=3:deblock=-1,-1"]
POSTER_QUALITY = ["-q:v", "3"]  # provisional: today's feel-poster.jpg is 278 KB; the size gate bounds it


def video_argv(ffmpeg, master, out, video):
    codec = AV1_CODEC if video.codec == "av1" else H264_CODEC
    return ([str(ffmpeg), "-hide_banner", "-nostdin", "-loglevel", "warning", "-stats", "-y", "-i", str(master),
             "-map", "0:v:0", "-an", "-fps_mode", "passthrough", "-vf", video_filter(video.width, video.height)]
            + codec + COLOR_TAGS + ["-movflags", "+faststart", str(out)])


def poster_argv(ffmpeg, master, out, poster):
    return ([str(ffmpeg), "-hide_banner", "-nostdin", "-loglevel", "warning", "-y", "-i", str(master),
             "-map", "0:v:0", "-an", "-frames:v", "1", "-update", "1", "-vf", poster_filter(poster.width, poster.height)]
            + POSTER_QUALITY + [str(out)])


def encode_plan(ffmpeg, master, out_dir):
    """[(output name, argv)] for the four videos and two posters; pure, so a test asserts the argv."""
    out_dir = pathlib.Path(out_dir)
    plan = [(v.name, video_argv(ffmpeg, master, out_dir / v.name, v)) for v in VIDEOS]
    plan += [(p.name, poster_argv(ffmpeg, master, out_dir / p.name, p)) for p in POSTERS]
    return plan


def run_encode(ffmpeg, master, frames_tsv, out_dir, render=None, run=subprocess.run):
    out_dir = pathlib.Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    rows = parse_frames_tsv(pathlib.Path(frames_tsv).read_text())
    assert_encoders(ffmpeg, run)
    for name, argv in encode_plan(ffmpeg, master, out_dir):
        print(f"site_video: encode {name}", flush=True)
        done = run(argv)
        if done.returncode != 0:
            (out_dir / name).unlink(missing_ok=True)
            raise Failure(f"encode {name}: ffmpeg exited {done.returncode}")
    shutil.copyfile(frames_tsv, out_dir / "frames.tsv")
    if render:
        shutil.copyfile(render, out_dir / "render.json")
    return len(rows)


# ------------------------------------------------------------------ frames.tsv and the render record
TSV_HEADER = "n\tt_ms\txxh3\tdebt\tholds"


def parse_frames_tsv(text):
    """[(n, t_ms, xxh3, debt, holds)], validated: the header, a gapless n from 0, 16 hex digits."""
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    if not lines or lines[0] != TSV_HEADER:
        raise Failure(f"frames.tsv: the first line must be exactly {TSV_HEADER!r}")
    rows = []
    for i, line in enumerate(lines[1:]):
        cols = line.split("\t")
        if len(cols) != 5:
            raise Failure(f"frames.tsv row {i}: {len(cols)} columns, want 5")
        try:
            n, t_ms, debt, holds = int(cols[0]), int(cols[1]), int(cols[3]), int(cols[4])
        except ValueError:
            raise Failure(f"frames.tsv row {i}: n, t_ms, debt and holds are integers: {line!r}")
        if n != i:
            raise Failure(f"frames.tsv row {i}: n is {n}; the written-frame index runs 0, 1, 2 ... with no gap")
        if not re.fullmatch(r"[0-9a-f]{16}", cols[2]):
            raise Failure(f"frames.tsv row {i}: xxh3 is 16 lowercase hex digits, got {cols[2]!r}")
        if debt < 0 or holds < 0:
            raise Failure(f"frames.tsv row {i}: debt and holds are not negative")
        rows.append((n, t_ms, cols[2], debt, holds))
    if not rows:
        raise Failure("frames.tsv has a header and no frames")
    return rows


# What S5b writes next to the master, as JSON. `dotted path -> (type, what)`.
RENDER_RECORD_FIELDS = {
    "schema": (int, f"{SCHEMA}"),
    "storyboard_sha256": (str, "sha256 of the storyboard JSON the run played"),
    "frames.count": (int, "written frames; equals the frames.tsv rows and the master's frames"),
    "frames.fps": (int, "60"),
    "frames.width": (int, "1920"),
    "frames.height": (int, "1080"),
    "placeholder_debt.frames_with_debt": (int, "written frames whose draw-site count was above 0; must be 0"),
    "sentinel.scanned_frames": (int, "frames the inline scanner saw; must equal frames.count"),
    "sentinel.hits": (int, "frames with a connected run of >= 64 px of #FF00FE; must be 0"),
    "hero_pool.logged": (list, "ratingKeys (strings) the Home hero actually rotated through"),
    "hero_pool.eligible": (list, "ratingKeys (strings) the hero-eligibility check admits (plan section 1.4)"),
    "opened_rating_keys": (list, "ratingKeys (strings) the storyboard really opened"),
}


def _dig(rec, dotted):
    cur = rec
    for part in dotted.split("."):
        if not isinstance(cur, dict) or part not in cur:
            return None, False
        cur = cur[part]
    return cur, True


def validate_render_record(rec):
    """Problems with the render record's shape; [] when it is well formed."""
    problems = []
    if not isinstance(rec, dict):
        return ["the render record is not a JSON object"]
    for dotted, (typ, _) in RENDER_RECORD_FIELDS.items():
        val, present = _dig(rec, dotted)
        if not present:
            problems.append(f"missing {dotted}")
        elif not isinstance(val, typ) or isinstance(val, bool):
            problems.append(f"{dotted} must be {typ.__name__}")
    if _dig(rec, "schema")[0] not in (None, SCHEMA):
        problems.append(f"schema must be {SCHEMA}")
    for dotted in ("hero_pool.logged", "hero_pool.eligible", "opened_rating_keys"):
        val, present = _dig(rec, dotted)
        if present and isinstance(val, list) and not all(isinstance(x, str) and x for x in val):
            problems.append(f"{dotted} holds ratingKeys as non-empty strings")
    return problems


# ------------------------------------------------------------------ gates -----------------------
Result = collections.namedtuple("Result", "name status detail")
PASS, FAIL, SKIP = "pass", "fail", "skip"


def gate_frames_tsv(rows, record, master_frames):
    """The tsv matches the master and the record, and no written frame carries placeholder debt."""
    out = []
    n = len(rows)
    counts = {"the master": master_frames, "render.json frames.count": _dig(record, "frames.count")[0]}
    bad = [f"{k} has {v}" for k, v in counts.items() if v != n]
    out.append(Result("frames/count", FAIL if bad else PASS,
                      f"frames.tsv has {n} rows but " + "; ".join(bad) if bad else f"{n} frames agree"))
    debt = [(r[0], r[3]) for r in rows if r[3] != 0]
    rec_debt = _dig(record, "placeholder_debt.frames_with_debt")[0]
    if debt or rec_debt != 0:
        out.append(Result("placeholder-debt", FAIL,
                          f"{len(debt)} written frames carry debt (first: frame {debt[0][0]} has {debt[0][1]}); "
                          f"render.json says {rec_debt}" if debt else f"render.json says {rec_debt} frames carry debt"))
    else:
        out.append(Result("placeholder-debt", PASS, f"0 of {n} written frames carry placeholder debt"))
    return out


def gate_sentinel_record(record):
    hits, scanned = _dig(record, "sentinel.hits")[0], _dig(record, "sentinel.scanned_frames")[0]
    count = _dig(record, "frames.count")[0]
    if scanned != count:
        return Result("sentinel/inline", FAIL, f"the inline scanner saw {scanned} frames of {count}: it did not cover the run")
    if hits != 0:
        return Result("sentinel/inline", FAIL, f"the inline scanner flagged {hits} frames")
    return Result("sentinel/inline", PASS, f"{scanned} frames scanned inline, 0 flagged")


def gate_run_twice(tsv_a, tsv_b):
    if tsv_b is None:
        return Result("run-twice", FAIL, "no second frames.tsv supplied (--frames-b): the run-twice check is mandatory")
    a, b = sha256_file(tsv_a), sha256_file(tsv_b)
    if a != b:
        ra, rb = parse_frames_tsv(pathlib.Path(tsv_a).read_text()), parse_frames_tsv(pathlib.Path(tsv_b).read_text())
        first = next((i for i, (x, y) in enumerate(zip(ra, rb)) if x != y), None)
        where = f"first differing frame {first}" if first is not None else f"{len(ra)} vs {len(rb)} frames"
        return Result("run-twice", FAIL, f"the two renders differ ({where}); sha256 {a[:12]} != {b[:12]}")
    return Result("run-twice", PASS, f"both renders hash to {a[:12]}")


def gate_hero_pool(record):
    logged = _dig(record, "hero_pool.logged")[0] or []
    eligible = set(_dig(record, "hero_pool.eligible")[0] or [])
    if not logged:
        return Result("hero-pool", FAIL, "no hero was logged: nothing proves the pool")
    extra = sorted(set(logged) - eligible)
    if extra:
        return Result("hero-pool", FAIL, f"hero ratingKeys not in the eligible set: {', '.join(extra)}")
    return Result("hero-pool", PASS, f"{len(set(logged))} logged heroes, all eligible")


def gate_sizes(out_dir, site_media, ratio=None):
    """Each file no larger than `ratio` times the file of the same name in site/media, read now."""
    ratio = ratio or THRESHOLDS["size_ratio"]
    out = []
    for name in MEDIA_NAMES:
        new, old = pathlib.Path(out_dir) / name, pathlib.Path(site_media) / name
        if not new.is_file():
            out.append(Result(f"size/{name}", FAIL, "not encoded"))
        elif not old.is_file():
            out.append(Result(f"size/{name}", SKIP, "no file of this name in site/media to compare with"))
        else:
            a, b = new.stat().st_size, old.stat().st_size
            ok = a <= ratio * b
            out.append(Result(f"size/{name}", PASS if ok else FAIL,
                              f"{a:,} bytes is {a / b:.2f}x today's {b:,} (limit {ratio}x)"))
    return out


# --- ffmpeg-backed measurements -------------------------------------------------------------------
_SSIM_LINE = re.compile(r"n:(\d+).*?All:([0-9.]+)")
_STREAM = re.compile(r"Stream #0:\d+\S*: Video: (\w+)(?: \(([^)]*)\))?.*?, (yuvj?\d+p\d*|gbrp|rgb\w+|bgr\w+|gray\w*)"
                     r"(?:\(([^)]*)\))?.*?, (\d+)x(\d+)")


def parse_ssim_stats(text):
    return [float(m.group(2)) for m in _SSIM_LINE.finditer(text)]


def probe(ffmpeg, path, run=subprocess.run):
    """codec, profile, pix_fmt, colour words, size, fps, frame count and whether there is audio,
    from `ffmpeg -i` (so the tool needs no ffprobe)."""
    done = run([str(ffmpeg), "-hide_banner", "-nostdin", "-i", str(path), "-map", "0", "-c", "copy", "-f", "null", "-"],
               capture_output=True, text=True, timeout=1800)
    return parse_probe(done.stderr)


def parse_probe(stderr):
    info = {"audio": bool(re.search(r"Stream #0:\d+.*: Audio:", stderr))}
    m = _STREAM.search(stderr)
    if not m:
        raise Failure("could not read a video stream line from ffmpeg's output")
    info.update(codec=m.group(1), profile=m.group(2), pix_fmt=m.group(3), color=m.group(4) or "",
                width=int(m.group(5)), height=int(m.group(6)))
    fps = re.search(r"([\d.]+) fps", stderr[m.start():].split("\n", 1)[0])
    info["fps"] = float(fps.group(1)) if fps else None
    frames = re.findall(r"frame=\s*(\d+)", stderr)
    info["frames"] = int(frames[-1]) if frames else None
    return info


def gate_format(info, name, expect, frames):
    """The container carries what the site's `codecs` strings promise: size, 8-bit 4:2:0, BT.709 TV range
    (video), 60 fps, no audio, and exactly the master's frame count."""
    problems = []
    if isinstance(expect, Video):
        want_codec = expect.codec
        if info["codec"] != want_codec:
            problems.append(f"codec {info['codec']}, want {want_codec}")
        if info["pix_fmt"] != "yuv420p":
            problems.append(f"pix_fmt {info['pix_fmt']}, want yuv420p")
        for word in ("tv", "bt709"):
            if word not in info["color"]:
                problems.append(f"colour tags {info['color']!r} lack {word}")
        if info["fps"] is not None and abs(info["fps"] - FPS) > 0.01:
            problems.append(f"{info['fps']} fps, want {FPS}")
        if info["frames"] != frames:
            problems.append(f"{info['frames']} frames, want {frames}")
        if info["audio"]:
            problems.append("has an audio stream (the video is muted: -an)")
    else:
        if info["codec"] != "mjpeg":
            problems.append(f"codec {info['codec']}, want mjpeg")
        if info["pix_fmt"] != "yuvj420p":
            problems.append(f"pix_fmt {info['pix_fmt']}, want yuvj420p")
    if (info["width"], info["height"]) != (expect.width, expect.height):
        problems.append(f"{info['width']}x{info['height']}, want {expect.width}x{expect.height}")
    return Result(f"format/{name}", FAIL if problems else PASS, "; ".join(problems) or "format as the site expects")


def ssim_against_master(ffmpeg, dist, master, ref_chain, dist_format, run=subprocess.run, frames=None):
    """Per-frame SSIM of `dist` against the master put through `ref_chain` (the encoder's own input)."""
    with tempfile.TemporaryDirectory() as tmp:
        cut = f"trim=end_frame={frames}," if frames else ""  # a poster is ONE frame: frame 0
        lavfi = f"[0:v]{cut}format={dist_format}[d];[1:v]{cut}{ref_chain}[r];[d][r]ssim=stats_file=ssim.log"
        argv = [str(ffmpeg), "-hide_banner", "-nostdin", "-i", str(dist), "-i", str(master), "-lavfi", lavfi,
                "-f", "null", "-"]
        done = run(argv, cwd=tmp, capture_output=True, text=True, timeout=7200)
        log = pathlib.Path(tmp) / "ssim.log"
        if done.returncode != 0 or not log.is_file():
            raise Failure(f"ssim of {pathlib.Path(dist).name} failed: {done.stderr.strip()[-300:]}")
        values = parse_ssim_stats(log.read_text())
    if not values:
        raise Failure(f"ssim of {pathlib.Path(dist).name} produced no frames")
    return values


def gate_ssim(name, values, min_floor, mean_floor, frames=None):
    mn, mean = min(values), sum(values) / len(values)
    problems = []
    if frames is not None and len(values) != frames:
        problems.append(f"{len(values)} frames compared, master has {frames}")
    if mn < min_floor:
        problems.append(f"worst frame {values.index(mn)} has SSIM {mn:.4f} < {min_floor}")
    if mean < mean_floor:
        problems.append(f"mean SSIM {mean:.4f} < {mean_floor}")
    return Result(f"ssim/{name}", FAIL if problems else PASS,
                  "; ".join(problems) or f"min {mn:.4f} (floor {min_floor}), mean {mean:.4f} (floor {mean_floor})")


def have_filter(ffmpeg, name, run=subprocess.run):
    out = run([str(ffmpeg), "-hide_banner", "-filters"], capture_output=True, text=True, timeout=60).stdout
    return re.search(rf"^\s*\S+\s+{re.escape(name)}\s", out, re.M) is not None


def vmaf_against_master(ffmpeg, dist, master, ref_chain, run=subprocess.run):
    with tempfile.TemporaryDirectory() as tmp:
        lavfi = (f"[0:v]format=yuv420p[d];[1:v]{ref_chain}[r];"
                 f"[d][r]libvmaf=log_fmt=json:log_path=vmaf.json:n_threads={os.cpu_count() or 2}")
        done = run([str(ffmpeg), "-hide_banner", "-nostdin", "-i", str(dist), "-i", str(master), "-lavfi", lavfi,
                    "-f", "null", "-"], cwd=tmp, capture_output=True, text=True, timeout=7200)
        log = pathlib.Path(tmp) / "vmaf.json"
        if done.returncode != 0 or not log.is_file():
            raise Failure(f"vmaf of {pathlib.Path(dist).name} failed: {done.stderr.strip()[-300:]}")
        metrics = json.loads(log.read_text())["pooled_metrics"]["vmaf"]
    return metrics["mean"], metrics["min"]


def gate_vmaf(name, mean, mn):
    problems = []
    if mean < THRESHOLDS["vmaf_mean"]:
        problems.append(f"mean VMAF {mean:.2f} < {THRESHOLDS['vmaf_mean']}")
    if mn < THRESHOLDS["vmaf_min"]:
        problems.append(f"min VMAF {mn:.2f} < {THRESHOLDS['vmaf_min']}")
    return Result(f"vmaf/{name}", FAIL if problems else PASS,
                  "; ".join(problems) or f"mean {mean:.2f}, min {mn:.2f} (provisional floors)")


def roi_box(width, height, fractions=None):
    x, y, w, h = fractions or THRESHOLDS["banding_roi"]
    return (int(width * x) // 2 * 2, int(height * y) // 2 * 2, max(2, int(width * w) // 2 * 2), max(2, int(height * h) // 2 * 2))


def flat_fraction(gray, width, height):
    """The share of horizontally and vertically adjacent pixel pairs, in one `width x height` 8-bit luma
    frame, whose values are equal. A smooth dark gradient quantised into long flat steps (banding)
    raises it."""
    same = total = 0
    rows = [gray[y * width:(y + 1) * width] for y in range(height)]
    for y, row in enumerate(rows):
        same += sum(map(operator.eq, row, row[1:]))
        total += width - 1
        if y + 1 < height:
            same += sum(map(operator.eq, row, rows[y + 1]))
            total += width
    return same / total


def roi_frames(ffmpeg, path, box, every, chain=None, run=subprocess.run):
    x, y, w, h = box
    vf = ",".join(filter(None, [chain, f"select='not(mod(n\\,{every}))'", f"crop={w}:{h}:{x}:{y}", "format=gray"]))
    done = run([str(ffmpeg), "-hide_banner", "-nostdin", "-i", str(path), "-vf", vf, "-fps_mode", "passthrough",
                "-f", "rawvideo", "-pix_fmt", "gray", "-"], capture_output=True, timeout=3600)
    if done.returncode != 0:
        raise Failure(f"banding probe of {pathlib.Path(path).name} failed: {done.stderr.decode(errors='replace')[-300:]}")
    size = w * h
    return [done.stdout[i:i + size] for i in range(0, len(done.stdout) - size + 1, size)]


def gate_banding(name, dist_frames, ref_frames, box):
    if not dist_frames or len(dist_frames) != len(ref_frames):
        return Result(f"banding/{name}", FAIL, f"probed {len(dist_frames)} frames against {len(ref_frames)} of the master")
    w, h = box[2], box[3]
    d = sum(flat_fraction(f, w, h) for f in dist_frames) / len(dist_frames)
    r = sum(flat_fraction(f, w, h) for f in ref_frames) / len(ref_frames)
    excess = d - r
    ok = excess <= THRESHOLDS["banding_excess"]
    return Result(f"banding/{name}", PASS if ok else FAIL,
                  f"flat-pair fraction {d:.3f} vs master {r:.3f}: excess {excess:+.3f} "
                  f"(limit {THRESHOLDS['banding_excess']}, provisional ROI {box})")


def seam_ssim(ffmpeg, path, frames, chain, run=subprocess.run):
    """SSIM of the first and the last frame of `path` (the loop's join)."""
    with tempfile.TemporaryDirectory() as tmp:
        pre = f"{chain}," if chain else ""
        lavfi = (f"[0:v]{pre}select='eq(n\\,0)',setpts=PTS-STARTPTS[a];"
                 f"[1:v]{pre}select='eq(n\\,{frames - 1})',setpts=PTS-STARTPTS[b];[a][b]ssim=stats_file=seam.log")
        done = run([str(ffmpeg), "-hide_banner", "-nostdin", "-i", str(path), "-i", str(path), "-lavfi", lavfi,
                    "-f", "null", "-"], cwd=tmp, capture_output=True, text=True, timeout=3600)
        log = pathlib.Path(tmp) / "seam.log"
        if done.returncode != 0 or not log.is_file():
            raise Failure(f"seam ssim of {pathlib.Path(path).name} failed: {done.stderr.strip()[-300:]}")
        values = parse_ssim_stats(log.read_text())
    if not values:
        raise Failure(f"seam ssim of {pathlib.Path(path).name} compared no frames")
    return values[0]


def gate_seam(name, value):
    floor = THRESHOLDS["seam_ssim"]
    return Result(f"seam/{name}", PASS if value >= floor else FAIL,
                  f"SSIM(first, last) {value:.4f} {'>=' if value >= floor else '<'} {floor}")


def gate_sentinel_master(ffmpeg, master, size, run=subprocess.run):
    """An independent rescan: decode the master to RGB24 and scan every frame."""
    w, h = size
    proc = subprocess.Popen([str(ffmpeg), "-hide_banner", "-nostdin", "-v", "error", "-i", str(master), "-map", "0:v:0",
                             "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], stdout=subprocess.PIPE)
    try:
        scanned, hit = scan_stream(proc.stdout, w, h)
    finally:
        proc.stdout.close()
        proc.wait()
    if hit:
        return Result("sentinel/master", FAIL, hit.message())
    if proc.returncode != 0:
        return Result("sentinel/master", FAIL, f"decoding the master failed (ffmpeg exit {proc.returncode})")
    return Result("sentinel/master", PASS, f"{scanned} master frames rescanned, none holds a connected "
                                           f"{SENTINEL_MIN_RUN}-pixel run of #FF00FE")


def run_gates(out_dir, master, frames_b=None, ffmpeg=None, site_media=SITE_MEDIA, run=subprocess.run,
              frames_a=None, render=None, progress=None):
    """Every gate of the plan's S6, each a named Result. `ffmpeg` is a path; the pure gates need none."""
    out_dir = pathlib.Path(out_dir)
    say = progress or (lambda msg: None)
    results = []
    try:
        tsv_path = pathlib.Path(frames_a or out_dir / "frames.tsv")
        rec_path = pathlib.Path(render or out_dir / "render.json")
        rows = parse_frames_tsv(tsv_path.read_text())
        record = json.loads(rec_path.read_text())
    except (Failure, OSError, ValueError) as e:
        return [Result("inputs", FAIL, str(e))]
    problems = validate_render_record(record)
    if problems:
        return [Result("render.json", FAIL, "; ".join(problems))]
    frames = len(rows)
    master_info = probe(ffmpeg, master, run)
    results.append(Result("master/format", PASS if (master_info["width"], master_info["height"]) == MASTER_SIZE
                          and master_info["codec"] == "ffv1" else FAIL,
                          f"{master_info['codec']} {master_info['width']}x{master_info['height']} {master_info['pix_fmt']}, "
                          f"{master_info['frames']} frames (want ffv1 {MASTER_SIZE[0]}x{MASTER_SIZE[1]})"))
    results += gate_frames_tsv(rows, record, master_info["frames"])
    results.append(gate_sentinel_record(record))
    say("rescanning the master for the sentinel")
    results.append(gate_sentinel_master(ffmpeg, master, MASTER_SIZE, run))
    results.append(gate_run_twice(tsv_path, frames_b))
    results.append(gate_hero_pool(record))
    results += gate_sizes(out_dir, site_media)
    vmaf = have_filter(ffmpeg, "libvmaf", run)
    for v in VIDEOS:
        path = out_dir / v.name
        if not path.is_file():
            results.append(Result(f"format/{v.name}", FAIL, "not encoded"))
            continue
        say(f"grading {v.name}")
        results.append(gate_format(probe(ffmpeg, path, run), v.name, v, frames))
        chain = video_filter(v.width, v.height)
        results.append(gate_ssim(v.name, ssim_against_master(ffmpeg, path, master, chain, "yuv420p", run),
                                 THRESHOLDS["ssim_min"], THRESHOLDS["ssim_mean"], frames))
        if vmaf:
            results.append(gate_vmaf(v.name, *vmaf_against_master(ffmpeg, path, master, chain, run)))
        else:
            results.append(Result(f"vmaf/{v.name}", SKIP, "libvmaf is not in this ffmpeg; VMAF NOT measured"))
        box = roi_box(v.width, v.height)
        results.append(gate_banding(v.name, roi_frames(ffmpeg, path, box, THRESHOLDS["banding_every"], None, run),
                                    roi_frames(ffmpeg, master, box, THRESHOLDS["banding_every"], chain, run), box))
        results.append(gate_seam(v.name, seam_ssim(ffmpeg, path, frames, None, run)))
    for p in POSTERS:
        path = out_dir / p.name
        if not path.is_file():
            results.append(Result(f"format/{p.name}", FAIL, "not encoded"))
            continue
        results.append(gate_format(probe(ffmpeg, path, run), p.name, p, frames))
        values = ssim_against_master(ffmpeg, path, master, poster_filter(p.width, p.height), "yuvj420p", run, frames=1)
        results.append(gate_ssim(p.name, values, THRESHOLDS["poster_ssim_min"], THRESHOLDS["poster_ssim_min"], 1))
    say("master loop seam")
    results.append(gate_seam("master", seam_ssim(ffmpeg, master, frames, video_filter(*MASTER_SIZE), run)))
    return results


def write_gates_json(out_dir, results):
    passed = all(r.status != FAIL for r in results)
    (pathlib.Path(out_dir) / "gates.json").write_text(json.dumps(
        {"schema": SCHEMA, "passed": passed, "thresholds": {k: list(v) if isinstance(v, tuple) else v
                                                            for k, v in THRESHOLDS.items()},
         "results": [r._asdict() for r in results]}, indent=1) + "\n")
    return passed


def format_results(results):
    width = max(len(r.name) for r in results)
    return "\n".join(f"{r.status.upper():4}  {r.name:<{width}}  {r.detail}" for r in results)


# ------------------------------------------------------------------ the sentinel scanner --------
SENTINEL = b"\xff\x00\xfe"  # #FF00FE in RGB24 byte order
SENTINEL_MIN_RUN = 64
_RUN = re.compile(rb"(?:\xff\x00\xfe)+")


class Hit(collections.namedtuple("Hit", "frame pixels bbox")):
    def message(self):
        x0, y0, x1, y1 = self.bbox
        return (f"frame {self.frame}: a connected run of {self.pixels} pixels of exactly #FF00FE "
                f"(bbox x {x0}-{x1}, y {y0}-{y1})")


def scan_frame(frame, width, height, min_run=SENTINEL_MIN_RUN):
    """(pixels, bbox) of the largest 4-connected region of exactly #FF00FE if it has at least `min_run`
    pixels, else None. `frame` is RGB24 bytes. One C-speed `in` rejects every clean frame; a frame that
    holds the three bytes anywhere then gets a per-row run scan and a union-find across rows.
    Connectivity is 4-way: a diagonal chain of single pixels is NOT a run, and neither is a scatter."""
    if len(frame) != width * height * 3:
        raise ValueError(f"frame is {len(frame)} bytes, want {width}x{height}x3")
    if SENTINEL not in frame:
        return None
    stride = width * 3
    parent, size, box = [], [], []
    prev = []  # [(x0, x1_exclusive, id)] of the previous row

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    for y in range(height):
        row = frame[y * stride:(y + 1) * stride]
        cur = []
        if SENTINEL in row:
            for m in _RUN.finditer(row):
                if m.start() % 3:  # a straddle of two pixels, never a pixel: it cannot overlap a true run
                    continue
                x0, x1 = m.start() // 3, m.end() // 3
                i = len(parent)
                parent.append(i)
                size.append(x1 - x0)
                box.append([x0, y, x1 - 1, y])
                for px0, px1, j in prev:
                    if px0 < x1 and x0 < px1:  # shares a column with a run above
                        a, b = find(i), find(j)
                        if a != b:
                            parent[b] = a
                            size[a] += size[b]
                            ba, bb = box[a], box[b]
                            box[a] = [min(ba[0], bb[0]), min(ba[1], bb[1]), max(ba[2], bb[2]), max(ba[3], bb[3])]
                cur.append((x0, x1, i))
        prev = cur
    best = None
    for i in range(len(parent)):
        if find(i) == i and size[i] >= min_run and (best is None or size[i] > size[best]):
            best = i
    return None if best is None else (size[best], tuple(box[best]))


def scan_stream(stream, width, height, tee=None, min_run=SENTINEL_MIN_RUN):
    """Scan RGB24 frames from a binary stream until the first hit. Returns (frames scanned, Hit or None).
    `tee` (a binary file) receives every frame that passed, so this can sit between a writer and ffmpeg."""
    size = width * height * 3
    n = 0
    while True:
        frame = stream.read(size)
        while frame and len(frame) < size:  # a pipe returns short reads
            more = stream.read(size - len(frame))
            if not more:
                raise Failure(f"the stream ended inside frame {n}: {len(frame)} of {size} bytes")
            frame += more
        if not frame:
            return n, None
        found = scan_frame(frame, width, height, min_run)
        if found:
            return n, Hit(n, found[0], found[1])
        if tee is not None:
            tee.write(frame)
        n += 1


# ------------------------------------------------------------------ the manifest ----------------
TREE_CRATES = ("ui", "screens", "data", "appkit")


def current_platform(env=None, system=None):
    """`linux-ci` only on a GitHub Actions Linux runner; anything else is a labelled local preview."""
    env = os.environ if env is None else env
    system = system or platform.system()
    if system == "Linux" and env.get("GITHUB_ACTIONS") == "true":
        return "linux-ci"
    return f"{system.lower()}-local"


def tree_hashes(run=subprocess.run, root=ROOT):
    """Git tree ids of the UI crates at HEAD, and whether the working tree differs from HEAD there."""
    out = {}
    paths = [f"rust-modules/{c}" for c in TREE_CRATES]
    for c, p in zip(TREE_CRATES, paths):
        done = run(["git", "-C", str(root), "rev-parse", f"HEAD:{p}"], capture_output=True, text=True)
        if done.returncode != 0:
            raise Failure(f"cannot read the git tree of {p}: {done.stderr.strip()}")
        out[c] = done.stdout.strip()
    dirty = run(["git", "-C", str(root), "status", "--porcelain", "--"] + paths, capture_output=True, text=True)
    out["combined"] = hashlib.sha256("".join(out[c] for c in TREE_CRATES).encode()).hexdigest()
    return out, bool(dirty.stdout.strip())


def file_entry(path):
    return {"sha256": sha256_file(path), "bytes": os.path.getsize(path)}


def build_manifest(out_dir, record, gates, ffmpeg_info, trees, dirty, catalog_sha, platform_label):
    out_dir = pathlib.Path(out_dir)
    missing = [n for n in MEDIA_NAMES if not (out_dir / n).is_file()]
    if missing:
        raise Failure(f"cannot write a manifest: missing {', '.join(missing)}")
    extras = sorted(p.name for p in out_dir.iterdir()
                    if p.is_file() and p.name not in MEDIA_NAMES and p.name != MANIFEST_NAME)
    if not gates.get("passed"):
        raise Failure("cannot write a manifest over a failed gates.json: fix the render and re-run `gates`")
    return {
        "schema": SCHEMA,
        "platform": platform_label,
        "outputs": {n: file_entry(out_dir / n) for n in MEDIA_NAMES},
        "artifacts": {n: file_entry(out_dir / n) for n in extras},
        "tools": ffmpeg_info,
        "storyboard_sha256": record["storyboard_sha256"],
        "catalog_sha256": catalog_sha,
        "tree_hash": trees,
        "tree_dirty": dirty,
        "opened_rating_keys": record["opened_rating_keys"],
        "hero_pool": record["hero_pool"],
        "render": record,
        "gates": {"passed": gates["passed"], "results": gates["results"]},
    }


def verify_manifest(manifest, directory, sections=("outputs", "artifacts")):
    """Problems (a list of strings) with a manifest against the files beside it; [] when it verifies.
    `sections=("outputs",)` is the committed copy beside site/media, which holds only the six media files."""
    problems = []
    if not isinstance(manifest, dict) or manifest.get("schema") != SCHEMA:
        return [f"not a schema-{SCHEMA} manifest"]
    directory = pathlib.Path(directory)
    for section in sections:
        for name, entry in sorted((manifest.get(section) or {}).items()):
            if name != os.path.basename(name) or name in ("", ".", ".."):
                problems.append(f"{section}: {name!r} is not a plain file name")
                continue
            path = directory / name
            if not path.is_file():
                problems.append(f"{name}: listed but absent")
            elif os.path.getsize(path) != entry.get("bytes"):
                problems.append(f"{name}: {os.path.getsize(path)} bytes, manifest says {entry.get('bytes')}")
            elif sha256_file(path) != entry.get("sha256"):
                problems.append(f"{name}: sha256 differs from the manifest")
    if sorted(manifest.get("outputs") or {}) != sorted(MEDIA_NAMES):
        problems.append(f"outputs must be exactly the six media files, got {sorted(manifest.get('outputs') or {})}")
    return problems


def make_artifact_zip(out_dir, manifest, zip_path):
    out_dir = pathlib.Path(out_dir)
    names = sorted(list(manifest["outputs"]) + list(manifest["artifacts"]))
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_STORED) as z:
        for name in names + [MANIFEST_NAME]:
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))  # fixed date: the zip is a function of its files
            info.compress_type = zipfile.ZIP_STORED
            info.external_attr = 0o644 << 16
            z.writestr(info, (out_dir / name).read_bytes())


def safe_extract(zip_path, dest, limit=1 << 30):
    dest = pathlib.Path(dest)
    with zipfile.ZipFile(zip_path) as z:
        total = 0
        for info in z.infolist():
            name = info.filename
            if (name.startswith("/") or "\\" in name or ".." in pathlib.PurePosixPath(name).parts
                    or (info.external_attr >> 16) & 0o170000 == 0o120000):
                raise Failure(f"the artifact holds an unsafe entry {name!r}")
            total += info.file_size
            if total > limit:
                raise Failure("the artifact is larger than 1 GiB uncompressed")
        z.extractall(dest)


def plan_adopt(zip_path, site_media, trees=None, tmp=None):
    """Verify an artifact and return [(src, dst)] to copy. Refuses anything not rendered on linux-ci, a
    failed or missing gates record, a dirty tree, or a file whose sha256 differs."""
    extracted = pathlib.Path(tmp)
    safe_extract(zip_path, extracted)
    mpath = extracted / MANIFEST_NAME
    if not mpath.is_file():
        raise Failure(f"the artifact has no {MANIFEST_NAME}")
    manifest = json.loads(mpath.read_text())
    problems = verify_manifest(manifest, extracted)
    if problems:
        raise Failure("manifest does not verify:\n  " + "\n  ".join(problems))
    if manifest.get("platform") != "linux-ci":
        raise Failure(f"refusing: the manifest's platform is {manifest.get('platform')!r}, not 'linux-ci'. "
                      f"Only the Linux CI render is canonical; a local render is a preview.")
    if not manifest.get("gates", {}).get("passed"):
        raise Failure("refusing: the manifest records failed or missing gates")
    if manifest.get("tree_dirty"):
        raise Failure("refusing: the render was made from a dirty tree")
    site_media = pathlib.Path(site_media)
    plan = [(extracted / n, site_media / n) for n in MEDIA_NAMES] + [(mpath, site_media / MANIFEST_NAME)]
    return plan, manifest


def adopt(zip_path, site_media=SITE_MEDIA, write=False, say=print, run=subprocess.run):
    with tempfile.TemporaryDirectory() as tmp:
        plan, manifest = plan_adopt(zip_path, site_media, tmp=tmp)
        try:
            here, _ = tree_hashes(run)
            same = here["combined"] == manifest["tree_hash"]["combined"]
            say("tree hash: " + ("matches HEAD" if same else "DIFFERS from HEAD (ui/screens/data/appkit changed "
                                                           "since the render): regenerate before release"))
        except Failure as e:
            say(f"tree hash: not compared ({e})")
        for src, dst in plan:
            say(f"{'copy' if write else 'would copy'} {dst.name}  ({src.stat().st_size:,} bytes)")
        if not write:
            say("dry run: nothing was written; pass --write to copy")
            return plan
        for src, dst in plan:
            tmp_dst = dst.with_name(dst.name + ".adopt-tmp")
            shutil.copyfile(src, tmp_dst)
            os.replace(tmp_dst, dst)
        return plan


# ------------------------------------------------------------------ contact sheet ---------------
def evenly_spaced(frames, count):
    """`count` frame indices spread over `frames`, first and last included."""
    if frames < 1 or count < 1:
        raise Failure("nothing to sample")
    if count == 1 or frames == 1:
        return [0]
    count = min(count, frames)
    return [round(i * (frames - 1) / (count - 1)) for i in range(count)]


def contact_sheet(ffmpeg, master, out, count=12, columns=4, tile_width=480, run=subprocess.run):
    try:
        from PIL import Image, ImageDraw, ImageFont
    except ImportError:
        raise Failure("contact-sheet needs Pillow: python3 -m pip install Pillow")
    info = probe(ffmpeg, master, run)
    picks = evenly_spaced(info["frames"], count)
    with tempfile.TemporaryDirectory() as tmp:
        select = "+".join(f"eq(n\\,{n})" for n in picks)
        done = run([str(ffmpeg), "-hide_banner", "-nostdin", "-v", "error", "-i", str(master), "-vf",
                    f"select='{select}',scale={tile_width}:-2:flags=lanczos", "-fps_mode", "passthrough",
                    "-frames:v", str(len(picks)), str(pathlib.Path(tmp) / "t%03d.png")],
                   capture_output=True, text=True, timeout=3600)
        tiles = sorted(pathlib.Path(tmp).glob("t*.png"))
        if done.returncode != 0 or len(tiles) != len(picks):
            raise Failure(f"contact sheet: extracted {len(tiles)} of {len(picks)} frames: {done.stderr.strip()[-300:]}")
        images = [Image.open(t).convert("RGB") for t in tiles]
        tw, th = images[0].size
        rows = -(-len(images) // columns)
        label_h, pad = 22, 6
        sheet = Image.new("RGB", (columns * (tw + pad) + pad, rows * (th + label_h + pad) + pad), (16, 16, 18))
        draw = ImageDraw.Draw(sheet)
        font = ImageFont.load_default()
        for i, (img, n) in enumerate(zip(images, picks)):
            x = pad + (i % columns) * (tw + pad)
            y = pad + (i // columns) * (th + label_h + pad)
            sheet.paste(img, (x, y + label_h))
            draw.text((x + 2, y + 5), f"frame {n}   t = {n / (info['fps'] or FPS):.2f} s", fill=(235, 235, 235), font=font)
        sheet.save(out, "JPEG", quality=88)
    return picks


# ------------------------------------------------------------------ command line ----------------
def _ffmpeg_from(args, run=subprocess.run):
    return resolve_ffmpeg(args.ffmpeg, args.cache, run)


def cmd_ffmpeg_fetch(args):
    pin = PINS[args.pin] if args.pin else PINS[host_pin_key()]
    path = fetch_ffmpeg(pin, args.cache)
    print(f"site_video: {pin.source}: GPL static build, sha256 {pin.sha256}", file=sys.stderr)
    print(path)


def cmd_encode(args):
    ffmpeg, _ = _ffmpeg_from(args)
    frames = run_encode(ffmpeg, args.master, args.frames, args.out, args.render)
    print(f"site_video: {len(MEDIA_NAMES)} files and frames.tsv ({frames} frames) in {args.out}")


def cmd_gates(args):
    ffmpeg, _ = _ffmpeg_from(args)
    results = run_gates(args.dir, args.master, args.frames_b, ffmpeg, args.site_media,
                        progress=lambda m: print(f"site_video: {m}", file=sys.stderr, flush=True))
    passed = write_gates_json(args.dir, results)
    print(format_results(results))
    skipped = [r.name for r in results if r.status == SKIP]
    if skipped:
        print(f"site_video: {len(skipped)} gate(s) SKIPPED: {', '.join(skipped)}", file=sys.stderr)
    return 0 if passed else 1


def cmd_scan(args):
    try:
        w, h = (int(x) for x in args.size.lower().split("x"))
    except ValueError:
        raise Failure("--size is WIDTHxHEIGHT, e.g. 1920x1080")
    src = sys.stdin.buffer if args.input in (None, "-") else open(args.input, "rb")
    tee = sys.stdout.buffer if args.tee else None
    try:
        n, hit = scan_stream(src, w, h, tee, args.min_run)
    finally:
        if tee is not None:
            tee.flush()
    if hit:
        print(f"site_video: SENTINEL: {hit.message()}", file=sys.stderr)
        return 1
    print(f"site_video: {n} frames scanned, no sentinel run", file=sys.stderr)
    return 0


def cmd_contact_sheet(args):
    ffmpeg, _ = _ffmpeg_from(args)
    picks = contact_sheet(ffmpeg, args.master, args.out, args.n, args.columns)
    print(f"site_video: {args.out}: frames {picks}")


def cmd_manifest(args):
    out_dir = pathlib.Path(args.dir)
    ffmpeg, canonical = _ffmpeg_from(args)
    record = json.loads((out_dir / "render.json").read_text())
    problems = validate_render_record(record)
    if problems:
        raise Failure("render.json: " + "; ".join(problems))
    gates_path = out_dir / "gates.json"
    if not gates_path.is_file():
        raise Failure("no gates.json: run `gates` first")
    label = current_platform()
    if record.get("platform") not in (None, label):
        raise Failure(f"render.json says platform {record['platform']!r} but this host is {label!r}")
    if STORYBOARD.is_file() and sha256_file(STORYBOARD) != record["storyboard_sha256"]:
        raise Failure("render.json's storyboard_sha256 differs from tests/video/feel.json")
    trees, dirty = tree_hashes()
    pin_hint = next((p.key for p in PINS.values() if ffmpeg.parent.name == f"ffmpeg-{p.sha256[:12]}"), None)
    tools = {"ffmpeg": {"sha256": sha256_file(ffmpeg), "version": ffmpeg_version_line(ffmpeg),
                        "canonical": canonical, "pin": pin_hint,
                        "archive_sha256": PINS[pin_hint].sha256 if pin_hint else None},
             "python": platform.python_version()}
    manifest = build_manifest(out_dir, record, json.loads(gates_path.read_text()), tools, trees, dirty,
                              sha256_file(CATALOG), label)
    (out_dir / MANIFEST_NAME).write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    print(f"site_video: wrote {out_dir / MANIFEST_NAME} (platform {label})")
    if args.zip:
        make_artifact_zip(out_dir, manifest, args.zip)
        print(f"site_video: wrote {args.zip}")


def cmd_verify(args):
    target = pathlib.Path(args.target)
    mpath = target / MANIFEST_NAME if target.is_dir() else target
    sections = ("outputs",) if args.outputs_only else ("outputs", "artifacts")
    problems = verify_manifest(json.loads(mpath.read_text()), mpath.parent, sections)
    if problems:
        raise Failure("manifest does not verify:\n  " + "\n  ".join(problems))
    print(f"site_video: {mpath} verifies")


def cmd_adopt(args):
    adopt(args.zip, args.site_media, args.write)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    tool = argparse.ArgumentParser(add_help=False)
    tool.add_argument("--cache", help="tool cache dir (default $PLXNATIVE_DEMO_CACHE or ~/.cache/plxnative-demo)")
    tool.add_argument("--ffmpeg", help="use THIS ffmpeg (loudly non-canonical) instead of the pinned build")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("ffmpeg-fetch", help="fetch the pinned static GPL ffmpeg; print its path", parents=[tool])
    p.add_argument("--pin", choices=sorted(PINS))
    p.set_defaults(fn=cmd_ffmpeg_fetch)
    p = sub.add_parser("encode", help="master -> the four encodes and two posters", parents=[tool])
    p.add_argument("master")
    p.add_argument("frames")
    p.add_argument("--out", required=True)
    p.add_argument("--render", help="the render record (render.json) to carry into --out")
    p.set_defaults(fn=cmd_encode)
    p = sub.add_parser("gates", help="grade the deliverables in DIR against the master", parents=[tool])
    p.add_argument("dir")
    p.add_argument("--master", required=True)
    p.add_argument("--frames-b", help="frames.tsv of the SECOND render (run-twice equality)")
    p.add_argument("--site-media", default=str(SITE_MEDIA))
    p.set_defaults(fn=cmd_gates)
    p = sub.add_parser("scan-sentinel", help="fail on a connected >=64 px run of #FF00FE in an RGB24 stream")
    p.add_argument("--size", required=True)
    p.add_argument("--tee", action="store_true", help="copy clean frames to stdout (sit between writer and ffmpeg)")
    p.add_argument("--min-run", type=int, default=SENTINEL_MIN_RUN)
    p.add_argument("input", nargs="?")
    p.set_defaults(fn=cmd_scan)
    p = sub.add_parser("contact-sheet", help="N evenly spaced labelled frames in one JPEG", parents=[tool])
    p.add_argument("master")
    p.add_argument("--out", required=True)
    p.add_argument("-n", type=int, default=12)
    p.add_argument("--columns", type=int, default=4)
    p.set_defaults(fn=cmd_contact_sheet)
    p = sub.add_parser("manifest", help="write feel.manifest.json for DIR (needs gates.json)", parents=[tool])
    p.add_argument("dir")
    p.add_argument("--zip", help="also write the artifact zip")
    p.set_defaults(fn=cmd_manifest)
    p = sub.add_parser("verify", help="check a manifest's sha256s against the files beside it")
    p.add_argument("target")
    p.add_argument("--outputs-only", action="store_true", help="only the six media files (the copy in site/media)")
    p.set_defaults(fn=cmd_verify)
    p = sub.add_parser("adopt", help="verify an artifact zip and copy it into site/media (dry run by default)")
    p.add_argument("zip")
    p.add_argument("--write", action="store_true")
    p.add_argument("--site-media", default=str(SITE_MEDIA))
    p.set_defaults(fn=cmd_adopt)
    args = ap.parse_args(argv)
    try:
        return args.fn(args) or 0
    except Failure as e:
        print(f"site_video: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
